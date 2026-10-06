"""tier_3 e2e for the region content plane's app side — region-blocking.md
§ The content plane → *How an app obtains its region's policy*, *The blocked
render and the transparency surface*, *What the build owes in tests*.

A synthetic region artifact, signed by a test-only authority enrolled in a
test-only registry (the ``region_relay_test_hook`` seed — never a real region),
sits in the nest's relay cache. The app declares the synthetic region through
the one test-capable-build-only override every app's leaf passes through
(``FAUNA_E2E_REGION_DECLARED``, keeping the leaf's own source — a test cannot
set a storefront or a user geo), and trusts the synthetic authority through the
registry seed (``FAUNA_E2E_REGION_REGISTRY``); both are convention 15 seams the
driver arranges (``declare_region_for_relaunch``). From there everything is the
production path: the app fetches
through ``fauna.region.artifact.get``, verifies in shared Rust, persists the
document on the device, and folds it as the third source of the render engine.

The journey:

1. The settings surface names the synthetic authority and the sequence.
2. A feed card a bundled ``list`` scorer names renders the reasoned placeholder
   in place of its body — frame, authority, reason verbatim — on that exact card;
   a card a ``collapse`` rule names shows the reveal, and the reveal shows the body.
3. A conversation bubble carrying the rule's canonical label renders the
   placeholder in place of its body.
4. The relay forgets the document; a cold relaunch still blocks — the device
   store, loaded ahead of the first fetch.
5. A document at a grammar version the app does not implement is inert, says so
   on the settings surface, and blocks nothing.

The feed posts and the DM label are fixture *preconditions* injected through the
existing test seams (the same ones the family-floor journeys use); the policy
the journey is about reaches the app only through the relay.
"""

from __future__ import annotations

import json
import time
import urllib.request

import pytest

from helpers.app_surface import app_name, skip_unbuilt
from helpers.authenticated_shell import wait_for_authenticated_shell
from i18n.strings import S

pytestmark = pytest.mark.tier_3

REGION = "XZ"
AUTHORITY = "Synthetic Test Authority"
BLOCK_REASON = "Withheld under the Synthetic Act, section 7."
COLLAPSE_REASON = "Hidden under the Synthetic Act, section 9."
DM_REASON = "Withheld under the Synthetic Act, section 11."

BLOCKED_POST = "b1" * 32
COLLAPSED_POST = "c2" * 32
CLEAN_POST = "a0" * 32


# Both helpers carry names no other function in the tree has: the mode-axis
# reach graph (`helpers/kind_reach.py`) is keyed on the BARE function name, so
# a generic `_seed` here merged with every other `_seed` and every caller of
# `launch`, and put this file's test-hook route onto tests that never touch it.
def _region_hook(nest_url: str, path: str, body: dict) -> dict:
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/region/{path}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=10) as resp:
        return json.loads(resp.read())


def _seed_region_policy(nest_url: str, sequence: int, version: int | None = None) -> str:
    body = {
        "region": REGION,
        "sequence": sequence,
        "rules": [
            {"verdict": "block", "reason": BLOCK_REASON, "content_ids": [BLOCKED_POST]},
            {"verdict": "collapse", "reason": COLLAPSE_REASON, "content_ids": [COLLAPSED_POST]},
            {"verdict": "block", "reason": DM_REASON, "factor": "nsfw"},
        ],
    }
    if version is not None:
        body["version"] = version
    reply = _region_hook(nest_url, "content-policy", body)
    assert reply["authority_name"] == AUTHORITY
    return reply["registry"]


# The apps whose region plane is built AND whose run of this journey is green;
# the rest skip as unbuilt until their leg of the trickle-down lands, joining
# the set with their first green run.
BUILT = {"tui", "linux", "web", "macos", "ios", "windows"}


def _relaunch_in_region(app, registry_hex: str) -> None:
    assert app.driver.declare_region_for_relaunch(REGION, registry_hex), (
        "the driver cannot relaunch with a declared region"
    )
    assert app.driver.recover(), "relaunch into the synthetic region failed"
    wait_for_authenticated_shell(app)


def _open_region_settings(app) -> None:
    # The section sits beside feature limits on the Status sub-page — its
    # canonical home on apps whose settings are a sub-page shell (linux, the
    # Apple apps); `test_feature_limits.py` reaches it the same way.
    app.settings.navigate()
    app.settings._navigate_subpage("status")
    try:
        app.driver.wait_for("settings-region-section", timeout=20)
    except TimeoutError as exc:
        # Convention 6: say WHERE the app is, so a never-rendered section and an
        # app that never reached its signed-in shell classify themselves.
        state = app.driver.get_state() or {}
        raise AssertionError(
            f"{exc}; session={state.get('session')!r} nav={state.get('nav')!r} "
            f"error={app.error_text()!r}"
        ) from exc


def _wait_policy_row(app, timeout: float = 30.0) -> None:
    """Wait for the relay fetch to land a policy on the settings surface. The
    surface is repainted on navigation, so re-open it until the row is there —
    a state wait, never a sleep-then-assert (convention 14)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        _open_region_settings(app)
        if app.driver.count("settings-region-policy-item") >= 1:
            return
        time.sleep(0.5)
    raise AssertionError(
        "no region policy reached the settings surface: "
        f"declared={app.driver.get_text('settings-region-declared')!r} "
        f"error={app.error_text()!r}"
    )


def _seed_feed(app) -> None:
    count = app.feed.seed_posts([
        {"post_id": CLEAN_POST, "author": "afriend", "body": "an ordinary post"},
        {"post_id": BLOCKED_POST, "author": "someone", "body": "this blocked body must never render"},
        {"post_id": COLLAPSED_POST, "author": "someone", "body": "a collapsed body behind the reveal"},
    ])
    assert count == 3, f"expected 3 seeded post-cards, got {count}. error: {app.error_text()!r}"


def _assert_feed_block(app) -> None:
    scope = "post-card[1]"
    assert app.driver.is_visible("region-blocked-notice", scope=scope), (
        f"the listed post did not render the region placeholder "
        f"({app.driver.diagnose('region-blocked-notice')}); error={app.error_text()!r}"
    )
    assert app.driver.get_text("region-blocked-notice", scope=scope) == S.region.blocked_notice(
        region=REGION, authority=AUTHORITY
    )
    assert app.driver.get_text("region-blocked-authority", scope=scope) == AUTHORITY
    assert app.driver.get_text("region-blocked-reason", scope=scope) == BLOCK_REASON
    assert app.driver.count("feed-post-text", scope=scope) == 0, "a blocked post renders no body"
    assert app.driver.count("region-collapsed-reveal-button", scope=scope) == 0, (
        "a region block has no reveal"
    )
    assert app.driver.is_absent("region-blocked-notice", scope="post-card[0]")
    assert app.driver.count("feed-post-text", scope="post-card[0]") >= 1


def test_region_policy_reaches_the_render_and_the_settings_surface(logged_in_app, nest_instance):
    app = logged_in_app
    if app_name(app.driver) not in BUILT:
        skip_unbuilt(
            app.driver,
            surface="region-blocked-notice",
            detail="the region content plane's six-app trickle-down has not reached this app yet",
            tracked="",
        )
    if not app.driver.preserve_state_across_relaunch():
        pytest.skip("driver cannot preserve the device store across a relaunch")

    try:
        registry_hex = _seed_region_policy(nest_instance["url"], sequence=1)
        _relaunch_in_region(app, registry_hex)

        # 1. The transparency surface: declared region + source, the policy,
        #    its authority and sequence.
        _wait_policy_row(app)
        assert app.driver.get_text("settings-region-declared") == S.region.declared(region=REGION)
        assert app.driver.get_text("settings-region-source") == getattr(
            S.region, app.driver.REGION_SOURCE_KEY
        )
        assert app.driver.get_text(
            "settings-region-policy-authority", scope="settings-region-policy-item[0]"
        ) == S.region.policy_authority(region=REGION, authority=AUTHORITY)
        version_text = app.driver.get_text(
            "settings-region-policy-version", scope="settings-region-policy-item[0]"
        )
        assert version_text.startswith("Version 1,"), version_text
        assert app.driver.count("settings-region-inert-notice") == 0

        # 2. The feed: block on the exact card, collapse with a reveal.
        _seed_feed(app)
        _assert_feed_block(app)
        collapsed = "post-card[2]"
        assert app.driver.get_text("region-blocked-notice", scope=collapsed) == (
            S.region.collapsed_notice(region=REGION, authority=AUTHORITY)
        )
        assert app.driver.get_text("region-blocked-reason", scope=collapsed) == COLLAPSE_REASON
        assert app.driver.count("feed-post-text", scope=collapsed) == 0
        app.driver.click("region-collapsed-reveal-button", scope=collapsed)
        app.driver.wait_for("feed-post-text", scope=collapsed, timeout=10)

        # 3. A conversation bubble carrying the rule's canonical label.
        app.conversations.navigate()
        app.conversations.inject_and_open_thread(
            rail="FaunaMls", sender="afriend-region@self-nest.test", body="an ordinary message"
        )
        app.conversations.inject_inbound_for_test(
            rail="FaunaMls",
            sender="afriend-region@self-nest.test",
            body="this nsfw body must never render",
            labels=[{"category": "nsfw", "confidence_per_mille": 900}],
        )
        deadline = time.monotonic() + 15.0
        while time.monotonic() < deadline and app.driver.count("region-blocked-notice") < 1:
            time.sleep(0.3)
        assert app.driver.count("region-blocked-notice") == 1, (
            f"the labelled DM did not render the region placeholder "
            f"({app.driver.diagnose('region-blocked-notice')}); error={app.error_text()!r}"
        )
        assert app.driver.get_text("region-blocked-reason", index=0) == DM_REASON
        assert app.driver.count("dm-message-text") == 1, "only the clean body renders"

        # 4. The relay forgets; a cold relaunch still blocks from the device store.
        _region_hook(nest_instance["url"], "content-policy/retire", {"region": REGION})
        assert app.driver.recover(), "relaunch after the relay retire failed"
        wait_for_authenticated_shell(app)
        _open_region_settings(app)
        assert app.driver.count("settings-region-policy-item") == 1, (
            "the device store did not restore the policy at launch"
        )
        _seed_feed(app)
        _assert_feed_block(app)

        # 5. A newer document at an unimplemented grammar version: inert, says so,
        #    blocks nothing.
        _seed_region_policy(nest_instance["url"], sequence=2, version=99)
        assert app.driver.recover(), "relaunch for the inert document failed"
        wait_for_authenticated_shell(app)
        deadline = time.monotonic() + 30.0
        while time.monotonic() < deadline:
            _open_region_settings(app)
            if app.driver.count("settings-region-inert-notice") >= 1:
                break
            time.sleep(0.5)
        assert app.driver.get_text(
            "settings-region-inert-notice", scope="settings-region-policy-item[0]"
        ) == S.region.inert_notice(version="99")
        _seed_feed(app)
        assert app.driver.count("region-blocked-notice") == 0, "an inert document blocks nothing"
        assert app.driver.count("feed-post-text", scope="post-card[1]") >= 1
    finally:
        app.driver.clear_region_declaration()
        app.driver.recover()
