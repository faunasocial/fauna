"""The AT Protocol history opt-in names the count it publishes
(docs/goal/behavior/atproto-pds-bridge.md § Projection & backfill: "Also
publish my N existing public posts").

The count is part of the consent — the user agrees to publish exactly that
many posts to a searchable network, irreversibly — so it is the nest's own
count of the posts the projection stream would serve, carried on the enable
panel's status read and composed into the label by the shared settings machine.

tier_3 (full stack — real `fauna-nest` binary): the user writes N public posts
through the app's composer, then enters a hosted level; the transition card's
opt-in must read N.

Its own module, deliberately: the opt-in rides only a *minting* transition, so
the module-scoped `atproto_hosted_nest` must not yet hold an identity for this
user — a module sharing that nest with a test that mints first would hide it.

tui, linux, web and android: tui led; windows and
apple render the composed label in their own trickle-down.
"""
import pytest

from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

POSTS = 3

# The module-scoped nest (and its user) outlives each app parametrization, so a
# later app's count includes every post an earlier one wrote.
_written = 0


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.feature("atproto")
def test_history_opt_in_names_the_public_post_count(atproto_hosted_logged_in_app):
    global _written
    app = atproto_hosted_logged_in_app
    app.feed.navigate()
    for i in range(POSTS):
        app.feed.create_post(f"backfill count post {_written + i}")
    _written += POSTS

    bs = app.atproto_settings
    bs.navigate()
    assert bs.is_page_visible(), f"atproto page not reachable. error: {app.error_text()!r}"
    assert bs.wait_for_depth_enabled("hosted_visible"), (
        f"hosted visible must be selectable on a public domain. error={bs.page_error_text()!r}"
    )
    bs.select_depth("hosted_visible")
    assert bs.wait_for_card(), (
        f"entering a hosted level must stage the transition card. error={bs.page_error_text()!r}"
    )

    expected = S.atproto_settings.history_backfill_count_label(count=str(_written))
    label = bs.history_backfill_label()
    assert label == expected, (
        f"the opt-in must name the {_written} public posts a backfill would publish. "
        f"label={label!r} error={bs.page_error_text()!r}"
    )
    bs.cancel_transition()
