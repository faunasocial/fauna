"""Your app's version, and news of a newer one — `docs/features/app-version-and-updates.md`.

Three outcomes, all walked on the Settings landing:

1. **Settings shows the version you are running** — the one product version
   every app and the nest share (`product-version.md` § The model). The walk
   reads `settings-app-version` and compares it with the workspace's own
   `Cargo.toml` version: the app must render the tree's version, never a
   hard-coded string or a per-app number.
2. **You can ask the app whether a newer version is out, and when one is, it
   says so and where to get it** (`installers/README.md` § Knowing a newer
   version is out). The walk presses `settings-check-updates-button` against
   the harness's stub release feed (`release_feed` marker →
   `helpers/release_feed_stub.py`), and asserts `update-available-notice`
   names the feed's tag AND the release page. The feed's origin is the only
   thing stubbed; the endpoint path, the JSON shape and the semver decision
   are the shared production ones, and the stub records that the app asked
   for exactly the production path. The walk signs in with the feed at the
   running version first, so the notice it then sees is the asked check's own.
3. **The app also looks once when you sign in, and only tells you** (the same
   section, amended 2026-10-03) — **at the nest it signed in to, never the
   release host** (§ *The nest's newest-release field*, 2026-10-10). A fresh
   sign-in against a nest reporting a newer release (seeded through the nest's
   `test-hooks` seam `/api/v1/test/release/newest`) paints the same notice with
   nobody pressing anything while the feed advertises only the running version,
   and makes no request to the feed; a sign-in against a nest that says nothing
   paints none and makes no request either. Both directions sign in a dedicated
   actor (`dedicated_actor_app`), so the look is that sign-in's own and never a
   leftover of an earlier test's. Every app that looks reads the nest (windows
   since 2026-11-04, the last to leave `fauna-ffi`'s retired release-host face).
   The catalog page does not list this outcome yet (its web, iOS and Android
   absences wait on the user's approval), so its witness carries no feature
   mark until it does.

**Columns.** Outcome 1 is owed on every app. Outcomes 2 and 3 are desktop + tui —
absent by design on web, ios and android (the page's `absences`). tui, linux,
macOS and windows carry all three ids and are walked (the About block is the
Settings root on tui, the General page on linux, macOS and windows —
`SettingsActions.navigate_to_about`).
"""

from __future__ import annotations

import json
import urllib.request

import pytest

from helpers.app_surface import declared_absence
from helpers.release_feed_stub import (
    LATEST_PATH,
    NEWER_TAG,
    RELEASE_REPO,
    running_tag,
    workspace_version as _workspace_version,
)
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.release_feed]

#: Where the update notice sends you (`fauna_core::version::release_page_url`).
RELEASE_PAGE = f"https://github.com/{RELEASE_REPO}/releases/tag/{NEWER_TAG}"


def _assert_names_the_newer_release(notice: str) -> None:
    version = NEWER_TAG.lstrip("v")
    assert version in notice, (
        f"the notice {notice!r} does not name the newer version {version!r} "
        f"(the feed advertised {NEWER_TAG})"
    )
    assert RELEASE_PAGE in notice, (
        f"the notice {notice!r} does not say where to get it — expected the release "
        f"page {RELEASE_PAGE}"
    )


#: The test-hooks seam that seeds the newest release the nest reports on
#: `fauna.nest.info` (`bins/fauna-nest/src/release_test_hook.rs`).
_NEWEST_RELEASE_HOOK = "/api/v1/test/release/newest"


def _seed_the_nests_newest_release(nest_url: str, version: str | None) -> None:
    req = urllib.request.Request(
        f"{nest_url}{_NEWEST_RELEASE_HOOK}",
        data=json.dumps({"version": version}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=10) as resp:
        resp.read()


def _nest_info_reads_since_the_seed(nest_url: str) -> int:
    with urllib.request.urlopen(f"{nest_url}{_NEWEST_RELEASE_HOOK}", timeout=10) as resp:
        return int(json.loads(resp.read())["reads"])


def _sign_in_with_the_nest_reporting(request, stub, newest: str | None):
    """Sign a dedicated actor in while the nest reports `newest` as the newest
    release it knows (`None`: it says nothing), with the release feed at the
    running version.

    Yields ``(app, nest_url, feed_requests_before)``: the feed count lets the
    walk tell whether the sign-in itself contacted the release host. The seed is
    set BEFORE the sign-in — the look runs at sign-in.
    """
    nest_url = request.getfixturevalue("nest_instance")["url"]
    _seed_the_nests_newest_release(nest_url, newest)
    with stub.advertising(running_tag()):
        before = stub.requests.count(LATEST_PATH)
        app, _user = request.getfixturevalue("dedicated_actor_app")
        yield app, nest_url, before


# Both request ``app`` by name — the sign-in is ``dedicated_actor_app``'s, on
# that same app — so the per-app parametrization reaches them and the catalog's
# static scan sees the driver they launch.
@pytest.fixture
def signed_in_with_nothing_newer(request, app, release_feed_stub):
    """A fresh sign-in against a nest that says nothing about releases."""
    yield from _sign_in_with_the_nest_reporting(request, release_feed_stub, None)


@pytest.fixture
def signed_in_with_a_newer_release_out(request, app, release_feed_stub):
    """A fresh sign-in against a nest reporting a newer release."""
    yield from _sign_in_with_the_nest_reporting(
        request, release_feed_stub, NEWER_TAG.lstrip("v")
    )


def _await_the_sign_in_look(app, nest_url: str) -> None:
    """Wait until the sign-in's look has reached the nest — it has answered
    `fauna.nest.info` with the seed at least once since it was set — then drain
    the app's UI work (convention 14's causal anchor)."""
    wait_until(
        lambda: _nest_info_reads_since_the_seed(nest_url) > 0,
        30,
        diagnose=lambda: "the nest answered no fauna.nest.info read after the sign-in",
    )
    app.driver.barrier()


def _skip_unless_the_update_ids_are_painted(app) -> None:
    driver = app.driver
    if driver.is_web() or driver.is_ios() or driver.is_android():
        declared_absence(
            driver,
            capability="the newer-version notice",
            doc="docs/goal/architecture/installers/README.md § Knowing a newer version is out",
        )


@pytest.mark.feature("app-version-and-updates")
def test_settings_shows_the_version_you_are_running(logged_in_app):
    app = logged_in_app
    app.settings.navigate_to_about()
    shown = app.settings.app_version()
    assert shown == _workspace_version(), (
        f"Settings shows version {shown!r}; the tree's product version is "
        f"{_workspace_version()!r} — every app and the nest render the ONE workspace "
        f"version (product-version.md § The model)."
    )


@pytest.mark.feature("app-version-and-updates")
def test_asking_for_a_newer_version_says_so_and_where_to_get_it(
    signed_in_with_nothing_newer, release_feed_stub
):
    app, nest_url, before = signed_in_with_nothing_newer
    _skip_unless_the_update_ids_are_painted(app)
    app.settings.navigate_to_about()

    # The sign-in looked and found nothing newer, so nothing painted — the
    # "nothing to tell" direction of outcome 3, and the clean slate the asked
    # check starts from.
    _await_the_sign_in_look(app, nest_url)
    assert app.driver.is_absent("update-available-notice"), (
        "a sign-in with nothing newer out painted the notice — the look must only "
        "tell you when a NEWER version is out"
    )
    # A nest that says nothing means no notice and no fall-back: the unasked
    # look never contacts the release host.
    assert release_feed_stub.requests.count(LATEST_PATH) == before, (
        "the sign-in contacted the release host although only the asked check "
        f"may; the stub saw {release_feed_stub.requests}"
    )

    with release_feed_stub.advertising(NEWER_TAG):
        app.settings.check_for_updates()
        _assert_names_the_newer_release(app.settings.update_notice())
    # The app asked the PRODUCTION endpoint — only the origin was stubbed.
    assert LATEST_PATH in release_feed_stub.requests, (
        f"the app never requested {LATEST_PATH}; the stub saw {release_feed_stub.requests}"
    )


# Outcome 3's witness; web, iOS and Android are declared absent from it.
@pytest.mark.feature("app-version-and-updates")
def test_signing_in_looks_once_and_only_tells_you(
    signed_in_with_a_newer_release_out, release_feed_stub
):
    app, nest_url, before = signed_in_with_a_newer_release_out
    _skip_unless_the_update_ids_are_painted(app)
    app.settings.navigate_to_about()

    # Nobody presses the check: the sign-in's own look paints the notice — from
    # the newest release the nest reports, while the release feed advertises only
    # the running version.
    _assert_names_the_newer_release(app.settings.update_notice(timeout=30))
    _await_the_sign_in_look(app, nest_url)
    looks = release_feed_stub.requests.count(LATEST_PATH) - before
    assert looks == 0, (
        f"one sign-in made {looks} release-host requests — the unasked look reads "
        "the nest the app signed in to and contacts no third party"
    )


def _running_binary(driver):
    """The executable this launch ran, where the driver can say (tui's
    ``app_path()``, linux's ``app_path()``); None where it cannot (a bundle,
    a browser). Unwraps the accessor however the driver exposes it."""
    import os

    path = getattr(driver, "app_path", None)
    for _ in range(3):
        if not callable(path):
            break
        path = path()
    return path if isinstance(path, (str, os.PathLike)) else None


def _file_digest(path) -> str | None:
    import hashlib
    import os

    if not path or not os.path.isfile(path):
        return None
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def _downloaded_files(driver) -> list[str]:
    import os

    root = getattr(driver, "download_dir", None)
    if callable(root):
        root = root()
    if not isinstance(root, (str, os.PathLike)) or not os.path.isdir(root):
        return []
    return sorted(
        os.path.relpath(os.path.join(d, f), root)
        for d, _dirs, files in os.walk(root) for f in files
    )


@pytest.mark.feature("app-version-and-updates")
def test_a_newer_version_is_only_announced_never_fetched_or_installed(
    signed_in_with_a_newer_release_out, release_feed_stub
):
    """Outcome 4 — the app never downloads or installs a newer version by
    itself; it only tells you one is out (`installers/README.md` § Knowing a
    newer version is out: no download, no self-replacement).

    With a newer release out — reported by the nest at sign-in AND advertised
    by the feed to an asked check — the notice paints from both, and then:
    the release host was asked for nothing but the release record (the stub
    404s and records every other path, an asset fetch included); nothing
    landed in the app's download directory; the running binary on disk is
    byte-identical; the same process is still running; and Settings still
    shows the version that is running.
    """
    app, nest_url, _before = signed_in_with_a_newer_release_out
    _skip_unless_the_update_ids_are_painted(app)
    driver = app.driver
    binary = _running_binary(driver)
    digest_before = _file_digest(binary)
    if getattr(driver, "is_tui", lambda: False)():
        assert digest_before is not None, (
            f"the tui driver names no running binary to watch ({binary!r}) — the "
            "self-replacement check below would silently not run"
        )
    downloads_before = _downloaded_files(driver)
    app.settings.navigate_to_about()

    # The unasked sign-in look's notice, then the asked check against a feed
    # advertising the newer tag.
    _await_the_sign_in_look(app, nest_url)
    with release_feed_stub.advertising(NEWER_TAG):
        app.settings.check_for_updates()
        _assert_names_the_newer_release(app.settings.update_notice())
        driver.barrier()

    stray = [p for p in release_feed_stub.requests if p != LATEST_PATH]
    assert not stray, (
        f"the app fetched {stray} from the release host — it may read the release "
        "record and nothing else (no asset, no installer)"
    )
    assert _downloaded_files(driver) == downloads_before, (
        "a newer version's notice wrote files into the download directory: "
        f"{_downloaded_files(driver)}"
    )
    if digest_before is not None:
        assert _file_digest(binary) == digest_before, (
            f"the running app's binary {binary} changed on disk after a newer "
            "version was announced — the app must never replace itself"
        )
    if hasattr(driver, "is_app_alive"):
        assert driver.is_app_alive(), (
            "the app stopped after announcing a newer version — it must keep running "
            "the version it is"
        )
    assert app.settings.app_version() == _workspace_version(), (
        "Settings must keep showing the running version after a newer one is announced"
    )
