"""Seeding for journeys that act on ANOTHER actor's Profile.

Lifted out of ``test_profile.py`` when the private contact overlay's journey
(``test_contact_overlay.py``) needed the same seeding — one home, never two
copies (priority #2). The accepted contact edge is the only wired tap-through
to another actor's Profile, and it is nest-side account state, so every seat of
the viewer's account sees the same row.
"""
from __future__ import annotations

from actions.api_actor import ApiActor
from common.auth import create_actor_and_register


def seed_accepted_contact(nest_instance: dict, user: dict) -> str:
    """Register a headless OTHER actor and have ``user`` accept them as a
    contact, through the API (arrangement, not the journey under test).
    Returns the OTHER actor's hex actor id."""
    other = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    viewer_actor = ApiActor(
        nest_instance["url"], user["token"], user["actor_id_hex"],
        bytes(user["signing_key"]),
    )
    viewer_actor.accept_knock(other["actor_id_hex"])
    return other["actor_id_hex"]


def open_other_profile(app, other_actor_hex: str) -> None:
    """Tap the contact row for ``other_actor_hex`` and wait for their Profile."""
    app.contacts.open_contact_profile(other_actor_hex)
    app.driver.wait_for("profile-view", timeout=10.0)


def open_new_contact_profile(app, nest_instance: dict, user: dict) -> str:
    """:func:`seed_accepted_contact`, then :func:`open_other_profile` — the
    OTHER header falls back to the actor id (no headless profile signing
    needed). Returns the OTHER actor's hex actor id."""
    other = seed_accepted_contact(nest_instance, user)
    open_other_profile(app, other)
    return other
