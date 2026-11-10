"""tier_3 e2e: **the owner's share picker shares a folder with a person on
another nest** — the owner-side half of `ui/folders.md` § Sharing a folder
(cross-user), the *Cross-nest members* bullet.

`test_folder_cross_nest_foreign_row.py` proves the RECIPIENT side of a
cross-nest share, but its sharer is headless: the welcome is minted and relayed
by API calls. Until this test's build no app's picker could originate one:
every app resolved the typed handle against its home nest alone and passed no
member nest URL to `FoldersAuthor::share_set`, so `user@<peer>` either failed
to resolve or failed at the owner's KeyPackage fetch.

The journey, entirely through the owner's app UI (convention 8):

1. Alice is signed in on the HOME nest. Bob is a handled actor on the FOREIGN
   nest (`cross_nest_foreign`, its own loopback authority as handle domain),
   with a published KeyPackage. Precondition setup, done headlessly: Bob never
   comes online.
2. Alice creates a folder, opens Share… and types `bob@<foreign authority>`.
   The picker classifies it with the shared `classify_recipient`, resolves it
   through the shared `find_user_across_nests` (the home probe, the
   foreign-domain rule, the anonymous lookup on the peer), and carries the
   found `nest_url` as `member_nest_url` into `FoldersAuthor::share_set`.
3. The home nest relays the KeyPackage fetch to the foreign nest, the set's
   channel binds on the home nest, and the Folder Welcome relays over the
   federation channel.

**The assertion is on BOB'S nest:** his inbox there holds the Welcome. A share
that dropped the URL would fail at the KeyPackage fetch (Bob has no account on
the home nest) — and a bare-localpart share would resolve no-one at all. The
roster row (`members.list_actors` unions `channel_foreign_members`) is the
owner-side half.

Exclusion class (8), as `test_contacts_cross_nest_knock.py`: the home nest
dials the foreign one, which in docker mode is refused as non-global; the read
of the foreign handle's `peer_url` declares it.

Built on all seven apps (tui first, the other six in the trickle-down; windows
last, 2026-11-07), so no surface key gates it any more.

tier_3: two real `fauna-nest` binaries + the real app with a live MLS
engine.
"""

from __future__ import annotations

import secrets

import pytest

from helpers import budgets
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]


@pytest.mark.feature("share-a-folder")
def test_owner_shares_a_folder_with_a_person_on_another_nest(
    logged_in_app, nest_instance, test_user, cross_nest_foreign,
):
    """Sharing to ``bob@<other-nest>`` through the picker lands the Folder
    Welcome in Bob's inbox ON HIS OWN NEST and puts him on the roster."""
    from helpers.admin_wire import admit_adult
    from tests.api import conv_api

    app = logged_in_app

    foreign = cross_nest_foreign
    authority = foreign["authority"]
    # The authority typed into the picker is the one the home nest is asked to
    # dial, so the two must name the same peer (and the read marks the class).
    assert foreign["peer_url"].endswith(authority), (
        "the authority typed into the share picker must be the one the home "
        f"nest is asked to dial: typed host {authority!r}, peer_url "
        f"{foreign['peer_url']!r}"
    )

    # Bob: handled on the FOREIGN nest, fresh per test, with a real published
    # KeyPackage — the owner's share consumes one over the home→foreign relay.
    bob = admit_adult(
        foreign, f"xnbob{secrets.token_hex(4)}", domain=authority, token=True,
    )
    conv_api.keypackage_upload(
        foreign["port"],
        bob,
        conv_api.mint_key_packages(bytes(bob["signing_key"]), 2),
        scheme="https",
    )
    before = {item["id"] for item in conv_api.inbox(foreign["url"], bob)}

    # ── The journey, through the owner's UI ──────────────────────────────────
    b = app.backups
    name = f"xnshare-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    b.find_and_expand_folder(name)
    assert b.shared_member_count() == 0, "a freshly-created set starts unshared"

    b.open_share_dialog()
    b.share_recipient_typed(f"{bob['handle']}@{authority}")

    # ── The assertion: the Welcome is in Bob's inbox on HIS nest ─────────────
    arrived = wait_until(
        lambda: [
            item
            for item in conv_api.inbox(foreign["url"], bob)
            if item["id"] not in before
        ]
        or None,
        budgets.CROSS_NEST_S,
        diagnose=lambda: (
            "no Welcome reached Bob's inbox on the foreign nest — the share "
            "never left the home nest (a dropped member nest URL fails at the "
            f"KeyPackage fetch); app error={app.error_text()!r}"
        ),
    )
    assert len(arrived) == 1, (
        f"exactly one new inbox item (the Folder Welcome) expected on Bob's "
        f"nest; got {len(arrived)}"
    )

    # ── The owner-side half: Bob is on the roster ────────────────────────────
    wait_until(
        lambda: b.shared_member_count() == 1 or None,
        budgets.CROSS_NEST_S,
        diagnose=lambda: (
            f"the roster never showed the remote member; count="
            f"{b.shared_member_count()} app error={app.error_text()!r}"
        ),
    )
    assert not app.has_error(), (
        f"the cross-nest share surfaced an error banner: {app.error_text()!r}"
    )
