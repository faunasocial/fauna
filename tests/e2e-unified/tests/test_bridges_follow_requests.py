"""Cross-app Bridges — the fediverse follow requests waiting on the account,
approved or refused from the ActivityPub card.

``docs/goal/behavior/bridges.md`` § Follow requests: with *accept follows by
itself* off, the linked ActivityPub card lists each waiting request — the
requester's address and display name, **Approve** and **Refuse**, no confirm —
under a heading that carries the count. What the two verdicts send to the
requester's server is ``docs/goal/behavior/activitypub.md`` § Follow requests:
approving delivers an ``Accept``, refusing a ``Reject``.

The nest half is witnessed without an app by
``tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery``;
this file is the app-surface half — the user's two clicks on the rendered rows
are what resolve, and each in-test follower's inbox (``helpers/fake_follower.py``)
is the oracle for what reached the fediverse.

The nest is a dedicated AP build (``activitypub,test-hooks``) started by
``helpers.ap_nest.start_ap_nest``: delivery to a loopback follower needs the
``FAUNA_TEST_AP_ALLOW_LOOPBACK`` test hook, which only a nest this harness
spawns can carry — hence ``standalone_only``.

Setup (enabling federation, turning the setting off, the followers' signed
``Follow``\\ s) goes through the nest's own doors, as a remote server and the
user's settings would; the journey under test — read the list, approve one,
refuse the other — is driven only through the app UI (convention 8).
"""

from __future__ import annotations

import time

import pytest

from drivers.port_util import find_free_port
from helpers.ap_nest import AP_BRIDGE_ID, enable_ap, start_ap_nest
from helpers.fake_follower import FakeFollower
from i18n.strings import S
from tests.api import ws_api

pytestmark = [pytest.mark.tier_3, pytest.mark.standalone_only]


@pytest.fixture(scope="module")
def ap_binary():
    """fauna-nest with ``activitypub`` + ``test-hooks`` — the federation
    suite's build, replayed from the collection-time prebuild through
    conftest's memoized ensure layer rather than paid inside a test."""
    from conftest import _ensure_ap_nest_built
    return _ensure_ap_nest_built()


@pytest.fixture(scope="module")
def requests_nest(ap_binary, tmp_path_factory):
    """One AP-enabled nest for the module, its domain its own loopback
    authority. Each test registers its own user, so no test reads another's
    state."""
    # An android venue run leases the port from the range its tunnel carries
    # (`helpers.android_venue.nest_port`); every other run takes a free one.
    from helpers import android_venue
    port, release_port = android_venue.nest_port(find_free_port)
    try:
        nest = start_ap_nest(
            ap_binary,
            tmp_path_factory.mktemp("follow-requests-nest"),
            port,
            f"127.0.0.1:{port}",
        )
    except BaseException:
        release_port()
        raise
    try:
        yield nest
    finally:
        nest["proc"].kill()
        nest["proc"].wait()
        release_port()


@pytest.fixture
def requests_spa_url(static_dir, requests_nest):
    """Web-only SPA proxy onto ``requests_nest`` — the session ``spa_url``
    proxies only the shared nest (mirrors ``fediverse_spa_url``)."""
    from conftest import _serve_spa_proxy
    url, server = _serve_spa_proxy(static_dir, requests_nest["url"])
    yield url
    server.shutdown()


def _send_follow(nest, follower: FakeFollower, username: str, actor_url: str) -> None:
    """Deliver ``follower``'s signed ``Follow`` to the account's inbox."""
    code = follower.post_signed(
        f"{nest['url']}/ap/users/{username}/inbox",
        {
            "@context": "https://www.w3.org/ns/activitystreams",
            "type": "Follow",
            "id": f"{follower.actor_uri}/activities/follow-1",
            "actor": follower.actor_uri,
            "object": actor_url,
        },
    )
    assert code == 202, f"inbox did not accept the Follow: HTTP {code}"


def _wait_for(read, want, what: str, timeout: float = 30.0):
    """Poll ``read()`` until ``want(value)`` holds; the failure names the last
    value read (convention 6) and the wait is a deadline, never a settle-sleep
    (convention 14)."""
    deadline = time.monotonic() + timeout
    value = read()
    while not want(value):
        if time.monotonic() >= deadline:
            raise AssertionError(f"{what}: last read {value!r}")
        time.sleep(0.3)
        value = read()
    return value


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.feature("fediverse")
def test_follow_requests_are_listed_and_approved_or_refused_on_the_card(
    request, app, requests_nest
):
    """A held-back ``Follow`` shows as a row on the ActivityPub card, under a
    heading counting the waiting requests. **Approve** on one removes its row
    and lands a signed ``Accept`` on that follower's inbox; **Refuse** on the
    other removes its row and lands a ``Reject``; the heading ends at zero."""
    from conftest import _login_app_as, _make_user

    nest = requests_nest
    user = _make_user(nest)
    username, actor_url = enable_ap(nest, user)
    ws_api.bridge_set_settings(
        nest["port"], user, AP_BRIDGE_ID, {"auto_accept_follows": False}
    )

    with FakeFollower(find_free_port(), username="bob") as bob, FakeFollower(
        find_free_port(), username="carol"
    ) as carol:
        _send_follow(nest, bob, username, actor_url)
        _send_follow(nest, carol, username, actor_url)

        _login_app_as(
            app, request, nest, user,
            spa_url_fixture="requests_spa_url", verify_live_actor=True,
        )
        app.bridges.navigate()
        assert app.bridges.is_page_visible(), (
            f"the Bridges page never rendered; error={app.error_text()!r}"
        )

        rows = _wait_for(
            lambda: app.bridges.follow_request_rows(AP_BRIDGE_ID),
            lambda r: len(r) == 2,
            "both held-back follows should list as requests",
        )
        assert app.bridges.follow_requests_heading(AP_BRIDGE_ID) == (
            S.bridges.follow_requests(count="2")
        )
        for follower in (bob, carol):
            handle = f"@{follower.username}@127.0.0.1"
            matching = [r for r in rows if handle in r]
            assert len(matching) == 1, (handle, rows)
            assert follower.display_name in matching[0], (follower.display_name, rows)
        # Nothing was answered while the requests wait.
        assert bob.received == [] and carol.received == []

        # Approve bob's row, wherever the list put it.
        bob_index = next(i for i, r in enumerate(rows) if "@bob@127.0.0.1" in r)
        app.bridges.approve_follow_request(AP_BRIDGE_ID, bob_index)
        bob.wait_for("Accept")
        rows = _wait_for(
            lambda: app.bridges.follow_request_rows(AP_BRIDGE_ID),
            lambda r: len(r) == 1,
            "approving a request should remove its row",
        )
        assert "@carol@127.0.0.1" in rows[0], rows

        app.bridges.refuse_follow_request(AP_BRIDGE_ID, 0)
        carol.wait_for("Reject")
        _wait_for(
            lambda: app.bridges.follow_request_rows(AP_BRIDGE_ID),
            lambda r: r == [],
            "refusing a request should remove its row",
        )
        assert app.bridges.follow_requests_heading(AP_BRIDGE_ID) == (
            S.bridges.follow_requests(count="0")
        )
        assert [e["activity"].get("type") for e in carol.received] == ["Reject"], (
            carol.received
        )
