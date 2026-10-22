"""The update channel on admin-nest (``nest/update-channel.md`` § The UI;
placement ``admin.md`` § N Nest → *Update channel*), driven through the app UI
against a real nest.

The admin picks a channel with the section's radio + save; the status line is
the shared ``fauna_client_admin::admin_update_channel_view`` fold of the read,
and the nest agrees: ``fauna.admin.update_channel.get`` and
``/api/v1/health``'s ``update_channel`` name the same channel (convention 5 —
the API leg that tells a UI-side failure from a nest-side one).

* **Following and Hold.** The nest reads its channel's head from the harness
  registry (``helpers/oci_registry.py``): Dev's head is far ahead, so choosing
  Dev reads *Following Dev*; Stable's head is behind the running build, so
  choosing Stable reads the never-downgrade decision's Hold sentence with both
  versions — the box stays on its build until Stable reaches it.
* **Pinned.** A nest installed from a tag that is no channel follows none: the
  status says so, and choosing a channel through the UI is how it starts
  following one.

* **Not from the image.** A nest started without ``FAUNA_IMAGE_TAG`` (the
  installers' nest service, a Linux package) cannot enact a channel: the status
  says so with the running version and the controls are inert.

A nest that predates the choice (unknown-kind) cannot be booted here — no older
binary is built — so that render is pinned by the fold's and tui's unit tests.

**Why dedicated nests.** Each test changes the channel and names its own seed
and registry, so neither may ride the shared session nest.

tui is the lead app; the other six follow in their batched trickle-down.
"""

import json
import time
import urllib.request

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from conftest import _login_admin_as
from helpers.app_surface import app_name, skip_unbuilt
from i18n.strings import S

pytestmark = pytest.mark.tier_3

# The apps whose admin-nest renders the section. tui leads.
BUILT = {"tui", "linux", "web"}

# The harness registry's heads: Dev far ahead of any running build, Stable
# (the ``latest`` tag) behind every one.
DEV_HEAD = "99.0.0"
STABLE_HEAD = "0.0.1"


def _wait_for(predicate, timeout: float = 20.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.3)
    return predicate()


def _health(nest) -> dict:
    with urllib.request.urlopen(nest["url"].rstrip("/") + "/api/v1/health") as resp:
        return json.loads(resp.read())


def _get(nest) -> dict:
    key = nest["admin"]["signing_key"]
    with WsRpcAdminClient(
        nest["url"], actor_id=bytes(key.verify_key), signing_key=bytes(key)
    ) as admin:
        return admin.call("fauna.admin.update_channel.get", {})


def _require_built(app) -> None:
    if app_name(app.driver) not in BUILT:
        skip_unbuilt(
            app.driver,
            surface="admin-nest-update-channel-section",
            detail="the update channel section's six-app trickle-down has not reached this app yet",
            tracked="",
        )


@pytest.fixture(scope="module")
def channel_registry():
    from helpers.oci_registry import OciRegistry

    with OciRegistry() as registry:
        registry.synthetic("dev", DEV_HEAD)
        registry.synthetic("latest", STABLE_HEAD)
        yield registry


def _dedicated(request, nest_mode, tmp_path_factory, label: str, env: dict):
    from conftest import _start_dedicated_nest

    return _start_dedicated_nest(request, nest_mode, tmp_path_factory, label, extra_env=env)


@pytest.fixture(scope="module")
def channel_nest(request, nest_mode, tmp_path_factory, channel_registry):
    """Installed from the ``test`` tag (no ``test`` head in the registry), reading
    heads from the harness registry."""
    nest, cleanup = _dedicated(
        request, nest_mode, tmp_path_factory, "update-channel-ui",
        {
            "FAUNA_IMAGE_TAG": "test",
            **channel_registry.env,
        },
    )
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture(scope="module")
def pinned_channel_nest(request, nest_mode, tmp_path_factory):
    """Installed from a commit tag — it follows no channel."""
    nest, cleanup = _dedicated(
        request, nest_mode, tmp_path_factory, "update-channel-ui-pinned",
        {"FAUNA_IMAGE_TAG": "sha-abc1234"},
    )
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture(scope="module")
def packaged_channel_nest(request, nest_mode, tmp_path_factory):
    """Started with no image tag in its environment — the nest cannot enact a
    channel, so it reports ``enactable: false``."""
    nest, cleanup = _dedicated(
        request, nest_mode, tmp_path_factory, "update-channel-ui-packaged", {},
    )
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture
def channel_spa_url(static_dir, channel_nest):
    from conftest import _serve_spa_proxy

    url, server = _serve_spa_proxy(static_dir, channel_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def pinned_channel_spa_url(static_dir, pinned_channel_nest):
    from conftest import _serve_spa_proxy

    url, server = _serve_spa_proxy(static_dir, pinned_channel_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def packaged_channel_spa_url(static_dir, packaged_channel_nest):
    from conftest import _serve_spa_proxy

    url, server = _serve_spa_proxy(static_dir, packaged_channel_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def channel_admin_app(request, app, channel_nest):
    _login_admin_as(app, request, channel_nest,
                    spa_url_fixture="channel_spa_url", fixture_name="channel_admin_app")
    yield app


@pytest.fixture
def pinned_channel_admin_app(request, app, pinned_channel_nest):
    _login_admin_as(app, request, pinned_channel_nest,
                    spa_url_fixture="pinned_channel_spa_url",
                    fixture_name="pinned_channel_admin_app")
    yield app


@pytest.fixture
def packaged_channel_admin_app(request, app, packaged_channel_nest):
    _login_admin_as(app, request, packaged_channel_nest,
                    spa_url_fixture="packaged_channel_spa_url",
                    fixture_name="packaged_channel_admin_app")
    yield app


def _choose(app, nest, channel: str, want_status: str) -> None:
    """Choose ``channel`` through the UI and wait for the status, the read and
    health to agree."""
    app.admin.select_update_channel(channel)
    app.admin.save_update_channel()
    assert _wait_for(lambda: app.admin.update_channel_status_text() == want_status), (
        f"after saving {channel}: status {app.admin.update_channel_status_text()!r}, "
        f"want {want_status!r}; error: {app.error_text()!r}"
    )
    reply = _get(nest)
    assert reply["channel"] == channel, reply
    assert _health(nest)["update_channel"] == channel, "health must follow the choice at once"


@pytest.mark.feature("admin-nest")
def test_admin_moves_the_box_between_channels_and_the_status_follows_the_decision(
    channel_admin_app, channel_nest,
):
    app = channel_admin_app
    _require_built(app)
    version = _health(channel_nest)["version"]
    app.admin.navigate_nest()

    # The seed: Test, whose head the registry does not carry — Following.
    following_test = S.admin.nest_page.update_channel_status_following_test(version=version)
    assert _wait_for(lambda: app.admin.update_channel_status_text() == following_test), (
        f"a test-seeded nest must read Following Test; status "
        f"{app.admin.update_channel_status_text()!r}, error: {app.error_text()!r}"
    )

    # Dev's head is ahead of the box: Following Dev.
    _choose(app, channel_nest, "dev",
            S.admin.nest_page.update_channel_status_following_dev(version=version))
    assert _get(channel_nest).get("channel_head_version") == DEV_HEAD

    # Stable's head is behind the box: the Hold sentence, both versions.
    _choose(app, channel_nest, "stable",
            S.admin.nest_page.update_channel_status_hold_stable(running=version, head=STABLE_HEAD))
    assert _get(channel_nest).get("channel_head_version") == STABLE_HEAD


@pytest.mark.feature("admin-nest")
def test_a_pinned_box_follows_no_channel_until_one_is_chosen(
    pinned_channel_admin_app, pinned_channel_nest,
):
    app = pinned_channel_admin_app
    _require_built(app)
    nest = pinned_channel_nest
    version = _health(nest)["version"]
    assert _health(nest)["update_channel"] is None
    app.admin.navigate_nest()

    pinned = S.admin.nest_page.update_channel_status_pinned(version=version)
    assert _wait_for(lambda: app.admin.update_channel_status_text() == pinned), (
        f"a pinned nest must say it follows no channel; status "
        f"{app.admin.update_channel_status_text()!r}, error: {app.error_text()!r}"
    )

    # No registry is named, so Test's head is unknown — Following.
    _choose(app, nest, "test",
            S.admin.nest_page.update_channel_status_following_test(version=version))


@pytest.mark.feature("admin-nest")
@pytest.mark.standalone_only
def test_a_nest_the_image_does_not_run_says_so_and_its_controls_are_inert(
    packaged_channel_admin_app, packaged_channel_nest,
):
    """``standalone_only``: the Docker nest mode stamps the image tag itself, so
    only a standalone nest can be started without one."""
    app = packaged_channel_admin_app
    _require_built(app)
    nest = packaged_channel_nest
    version = _health(nest)["version"]
    assert _health(nest)["update_channel"] is None
    app.admin.navigate_nest()

    not_from_image = S.admin.nest_page.update_channel_status_not_from_image(version=version)
    assert _wait_for(lambda: app.admin.update_channel_status_text() == not_from_image), (
        f"a nest the image does not run must say so; status "
        f"{app.admin.update_channel_status_text()!r}, error: {app.error_text()!r}"
    )

    # Inert: no radio is marked and none can be chosen, and the save cannot
    # commit — the view's can_set is false, so each control reads disabled.
    for channel in ("stable", "test", "dev"):
        assert app.driver.is_disabled(f"admin-nest-update-channel-{channel}-radio"), channel
    assert app.driver.is_disabled("admin-nest-update-channel-save-button")

    # The nest agrees: nothing was set, and nothing follows.
    reply = _get(nest)
    assert reply["enactable"] is False, reply
    assert not reply.get("channel"), reply
    assert _health(nest)["update_channel"] is None
