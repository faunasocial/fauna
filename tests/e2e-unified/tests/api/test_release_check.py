"""Where a test nest's daily release check reads — ``installers/README.md``
§ Knowing a newer version is out → *Where a test nest's check reads*, on a real
``fauna-nest`` binary.

A ``test-hooks`` nest reads ``FAUNA_E2E_RELEASE_FEED_URL`` from its own launch
environment once at startup. Set, the daily check polls that origin — the
production path (``fauna_core::version::latest_release_api_url``) and the
production fold, so ``fauna.nest.info`` reports the newest release the origin
names. Unset, the check does not run at all: no request leaves the nest, no
``update check failed`` line, and ``newest_release`` stays absent until a test
seeds it. A release build has no such seam and polls the real release API,
which is why both tests are ``standalone_only``: a docker run's nest is a
release image (its one ``update check failed`` warn per boot is what
``test_docker_mode_reaches_no_outside_name.py`` reads).

The session nest names no origin, so the negative is read off it too: its log
carries no release-check line — the proof a standalone run's nests reach no
release host.
"""

from pathlib import Path

import pytest

from clients.ws_rpc_anon_client import WsRpcAnonClient
from helpers.release_feed_stub import LATEST_PATH, ReleaseFeedStub
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

#: The update loop's failure line (`fauna_update::updater`), and the release
#: API's own error text it would carry.
_RELEASE_CHECK_LINES = ("update check failed", "GitHub API")

#: The User-Agent the nest's check sends (`fauna_update::github`).
_NEST_AGENT = "fauna-update"


def _newest_release(nest) -> str | None:
    with WsRpcAnonClient(nest["url"]) as anon:
        return anon.call("fauna.nest.info", {}).get("newest_release")


@pytest.fixture(scope="module")
def release_feed():
    """A per-test feed advertising a version far above the tree's."""
    stub = ReleaseFeedStub(tag="v99.0.0").start()
    try:
        yield stub
    finally:
        stub.stop()


@pytest.fixture(scope="module")
def feed_nest(request, nest_mode, tmp_path_factory, release_feed):
    """A dedicated nest whose check polls the stub."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "release-check-feed",
        extra_env={"FAUNA_E2E_RELEASE_FEED_URL": release_feed.origin},
    )
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture(scope="module")
def no_feed_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest started as the session nest is — naming no origin — so
    the absence is read where no other test has seeded the nest's answer."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "release-check-none",
    )
    try:
        yield nest
    finally:
        cleanup()


@pytest.mark.standalone_only
def test_a_nest_naming_a_feed_reports_the_newest_release_it_names(feed_nest, release_feed):
    newest = wait_until(
        lambda: _newest_release(feed_nest) == "99.0.0",
        30,
        diagnose=lambda: (
            f"fauna.nest.info reports {_newest_release(feed_nest)!r}; "
            f"the feed saw {release_feed.agents}"
        ),
    )
    assert newest
    assert release_feed.agents == [(LATEST_PATH, _NEST_AGENT)], (
        f"the nest's check must request the one release path, once, as "
        f"{_NEST_AGENT!r}: {release_feed.agents}"
    )


@pytest.mark.standalone_only
def test_a_nest_naming_no_feed_runs_no_check(no_feed_nest, nest_instance):
    # The session nest has served the whole run so far, so its log is the
    # long-lived half of the negative; the dedicated nest's is the fresh half.
    assert _newest_release(no_feed_nest) is None, (
        "a test-hooks nest naming no FAUNA_E2E_RELEASE_FEED_URL must run no "
        "release check, so it knows of no newest release"
    )
    for nest, name in ((no_feed_nest, "the dedicated nest"), (nest_instance, "the session nest")):
        log = Path(nest["log_path"]).read_text(errors="replace")
        found = [line for line in log.splitlines()
                 if any(marker in line for marker in _RELEASE_CHECK_LINES)]
        assert not found, (
            f"{name} names no release feed, so it must send no release check; "
            f"its log ({nest['log_path']}) carries: {found[:3]}"
        )
