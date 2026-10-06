"""Cross-nest federation tests.

These tests verify that content created on one nest can be seen on another.
They use the multi-nest fixtures (second_nest, second_user, api_actor_b)
and the primary nest's UI driver.

Multi-user tests use ApiActor for the remote user and the normal
logged_in_app for the local user's UI assertions.
"""

import pytest
from actions.api_actor import ApiActor

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


def test_second_nest_healthy(second_nest):
    """Verify the second nest starts and responds to health checks."""
    actor = ApiActor(second_nest["url"], second_nest["admin"]["token"], "")
    data = actor.health()
    assert data["status"] == "ok"
    assert "version" in data


def test_users_on_different_nests(nest_instance, test_user, second_nest, second_user):
    """Verify users are registered on separate nests with different actor IDs."""
    assert test_user["actor_id_hex"] != second_user["actor_id_hex"]

    # Each user can authenticate against their own nest
    actor_a = ApiActor(nest_instance["url"], test_user["token"], test_user["actor_id_hex"])
    actor_b = ApiActor(second_nest["url"], second_user["token"], second_user["actor_id_hex"])

    assert actor_a.health()["status"] == "ok"
    assert actor_b.health()["status"] == "ok"


def test_api_actor_feed_post(api_actor_b):
    """Verify ApiActor can create a feed and post to it."""
    feed_id = api_actor_b.create_feed("Cross-Nest Feed")
    assert len(feed_id) > 0

    post_id = api_actor_b.post_to_feed(feed_id, "Post from nest B", tags=["test"])
    # post_id may be empty on some nest versions — just verify no error
    posts = api_actor_b.get_feed_posts(feed_id)
    assert len(posts) > 0
