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
   section, amended 2026-10-03). A fresh sign-in against a feed advertising a
   newer version paints the same notice with nobody pressing anything, from
   exactly one request; a sign-in against a feed at the running version paints
   none. Both directions sign in a dedicated actor (`dedicated_actor_app`), so
   the look is that sign-in's own and never a leftover of an earlier test's.
   The catalog page does not list this outcome yet (its web, iOS and Android
   absences wait on the user's approval), so its witness carries no feature
   mark until it does.

**Columns.** Outcome 1 is owed on every app. Outcomes 2 and 3 are desktop + tui —
absent by design on web, ios and android (the page's `absences`). tui and linux
carry all three ids and are walked (the About block is the Settings root on tui,
the General page on linux — `SettingsActions.navigate_to_about`). Windows and
macOS are not a paint away: windows has no version row and its `UpdateService`
keeps its own feed URL instead of the shared entry point; macOS shows no app
version and its check is Sparkle, which owes the reshape ruled 2026-10-03 and an
FFI face for `fauna_client::update_look`. So this walk `skip_unbuilt`s them
rather than marking the module tui + linux: the day each is built, the skip
falls away and the same walk witnesses it.
"""

from __future__ import annotations

import pytest

from helpers.app_surface import declared_absence, skip_unbuilt
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


def _sign_in_against_feed(request, stub, tag: str):
    """Sign a dedicated actor in while the feed advertises `tag`.

    Yields ``(app, looks)``: ``looks`` is how many feed requests the stub had
    served before this sign-in, so the walk can tell the sign-in's own look from
    every earlier one. The tag is set BEFORE the sign-in — the look runs at
    sign-in — and restored after.
    """
    with stub.advertising(tag):
        before = stub.requests.count(LATEST_PATH)
        app, _user = request.getfixturevalue("dedicated_actor_app")
        yield app, before


# Both request ``app`` by name — the sign-in is ``dedicated_actor_app``'s, on
# that same app — so the per-app parametrization reaches them and the catalog's
# static scan sees the driver they launch.
@pytest.fixture
def signed_in_with_nothing_newer(request, app, release_feed_stub):
    """A fresh sign-in against a feed advertising the running version."""
    yield from _sign_in_against_feed(request, release_feed_stub, running_tag())


@pytest.fixture
def signed_in_with_a_newer_release_out(request, app, release_feed_stub):
    """A fresh sign-in against a feed advertising a newer release."""
    yield from _sign_in_against_feed(request, release_feed_stub, NEWER_TAG)


def _await_the_sign_in_look(stub, before: int, app) -> None:
    """Wait for the sign-in's own look to have been answered, then drain the
    app's UI work (convention 14's causal anchor): the stub records a request
    only once its answer is written."""
    wait_until(
        lambda: stub.requests.count(LATEST_PATH) > before,
        30,
        diagnose=lambda: f"the app never looked at the feed after signing in; "
        f"the stub saw {stub.requests}",
    )
    app.driver.barrier()


#: Where the windows and macOS legs are captured.
_TRACKED = ""


def _skip_unless_the_update_ids_are_painted(app) -> None:
    driver = app.driver
    if driver.is_web() or driver.is_ios() or driver.is_android():
        declared_absence(
            driver,
            capability="the newer-version notice",
            doc="docs/goal/architecture/installers/README.md § Knowing a newer version is out",
        )
    if driver.is_windows() or driver.is_macos():
        skip_unbuilt(
            driver,
            surface="settings-check-updates-button / update-available-notice",
            detail="windows' check keeps its own feed URL and owes the sign-in look; "
                   "macOS's is Sparkle, owing the 2026-10-03 reshape and an FFI face "
                   "for the shared update_look — neither carries the ids yet",
            tracked=_TRACKED,
        )


@pytest.mark.feature("app-version-and-updates")
def test_settings_shows_the_version_you_are_running(logged_in_app):
    app = logged_in_app
    if app.driver.is_windows() or app.driver.is_macos():
        skip_unbuilt(
            app.driver,
            surface="settings-app-version",
            detail="neither windows' About section nor macOS shows the app's version "
                   "yet — a row to build, not an id to paint",
            tracked=_TRACKED,
        )
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
    app, before = signed_in_with_nothing_newer
    _skip_unless_the_update_ids_are_painted(app)
    app.settings.navigate_to_about()

    # The sign-in looked at a feed advertising the running version: nothing
    # newer, so nothing painted — the "same or older → nothing" direction of
    # outcome 3, and the clean slate the asked check starts from.
    _await_the_sign_in_look(release_feed_stub, before, app)
    assert app.driver.is_absent("update-available-notice"), (
        "a sign-in against a feed at the running version painted the notice — the "
        "look must only tell you when a NEWER version is out"
    )

    with release_feed_stub.advertising(NEWER_TAG):
        app.settings.check_for_updates()
        _assert_names_the_newer_release(app.settings.update_notice())
    # The app asked the PRODUCTION endpoint — only the origin was stubbed.
    assert LATEST_PATH in release_feed_stub.requests, (
        f"the app never requested {LATEST_PATH}; the stub saw {release_feed_stub.requests}"
    )


# Outcome 3's witness, unmarked until the catalog lists that outcome: listing it
# declares web, iOS and Android absent from it, which waits on the user's
# approval. The answer lands the
# outcome and adds `@pytest.mark.feature("app-version-and-updates")` here.
def test_signing_in_looks_once_and_only_tells_you(
    signed_in_with_a_newer_release_out, release_feed_stub
):
    app, before = signed_in_with_a_newer_release_out
    _skip_unless_the_update_ids_are_painted(app)
    app.settings.navigate_to_about()

    # Nobody presses the check: the sign-in's own look paints the notice.
    _assert_names_the_newer_release(app.settings.update_notice(timeout=30))
    _await_the_sign_in_look(release_feed_stub, before, app)
    looks = release_feed_stub.requests.count(LATEST_PATH) - before
    assert looks == 1, (
        f"one sign-in made {looks} feed requests — the unasked look is ONE per "
        "sign-in, never a repeat or a timer"
    )
