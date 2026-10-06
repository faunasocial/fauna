"""Tier_3 "Export My Data" journey — the button delivers the full archive.

The account page's ``settings-export-data-button`` (ui.yaml settings page;
`docs/goal/ui/settings.md` § Data export) is the user's one affordance for
"give me everything you hold about me" (`principles.md` § The user always
controls their data). Decision (5) of `account-data-plane.md` § Nest-side
requirements item 1 makes the full archive the DEFAULT with no toggle: every
app fetches ``paths::account::EXPORT_FULL`` (``include_blobs=true``).

This is the journey test the tier_1 source assertion
(``test_account_export_carries_payload_bytes.py``) was standing in for while
the button had no element id (minted 2026-08-19): drive
the real button, capture the real download, and assert a well-formed archive
arrives — ``export/manifest.json`` present. The URL-level ``include_blobs``
guarantee stays with the tier_1 test: an index-only archive is a well-formed
zip too, so the archive alone cannot prove the flag — the requested URL can,
and the source assertion pins it.

Built on all 7 apps, windows the last leg, 2026-08-27.
"""

from __future__ import annotations

import io
import json
import secrets
import zipfile

import pytest

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("export-my-data")
def test_export_button_delivers_a_wellformed_archive(logged_in_app):
    app = logged_in_app
    data = app.settings.export_my_data()

    # Convention 6: put the page's own error text in the failure message —
    # the click dispatches the fetch into a task, so a failed export lands in
    # `error-message`, never in the click itself.
    err = app.get_text("error-message") if app.is_visible("error-message") else ""

    assert data[:2] == b"PK", (
        f"export did not deliver a zip (first bytes {data[:16]!r}, "
        f"{len(data)} bytes); error-message: {err!r}"
    )
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        names = z.namelist()
    assert "export/manifest.json" in names, (
        f"archive carries no export/manifest.json; first entries: "
        f"{sorted(names)[:20]}; error-message: {err!r}"
    )


@pytest.mark.feature("export-my-data")
def test_export_archive_carries_the_body_the_user_wrote(logged_in_app):
    """**Outcome 2** — *the archive holds your actual content — the message and
    post bodies, the files themselves — not just an index of what the nest has*.

    ``settings.md`` § Data export: "**One button, no options:** the archive
    always carries the user's payload bytes, and whether it does is not a choice
    the UI offers" — the ruling being ``account-data-plane.md`` § Nest-side
    requirements item 1, *Payload stores* decision (5). That is the whole point
    of the feature: an export that listed what the nest holds without handing it
    over would leave the user's data on the nest, which
    ``principles.md`` § The user always controls their data forbids.

    ``test_export_button_delivers_a_wellformed_archive`` deliberately stops
    short of this — its own header says "an index-only archive is a well-formed
    zip too, so the archive alone cannot prove the flag". It pinned the *shape*;
    the URL-level ``include_blobs`` guarantee went to the tier_1 source
    assertion (``test_account_export_carries_payload_bytes.py``), which launches
    no app and so cannot witness an ``[app]`` outcome. What neither asserts is
    the sentence itself: that something the user actually wrote comes back out.

    So this journey writes one, through the composer (convention 8 — the
    mutation is a real user action, not an API insert), and then looks for it
    inside the delivered archive. A **feed post** is the body to look for:
    ``export_routes`` writes post bodies to ``export/posts/<id>.json`` as
    ``data_hex`` through ``segments::post::load_post_body``, and they rest
    plaintext. Conversation bodies ride to ``export/conversations/bodies/…``
    still sealed — correctly, since the archive must not be a way to read what
    the nest itself cannot — so they can witness presence but never content, and
    a test asserting readable text would be asserting a bug.

    The marker is unique per run, so a green cannot be another test's leftover
    post and a red cannot be blamed on one.
    """
    marker = f"export-body-witness-{secrets.token_hex(8)}"
    logged_in_app.feed.create_post(marker)

    data = logged_in_app.settings.export_my_data()
    err = (
        logged_in_app.get_text("error-message")
        if logged_in_app.is_visible("error-message")
        else ""
    )
    assert data[:2] == b"PK", (
        f"export did not deliver a zip (first bytes {data[:16]!r}, "
        f"{len(data)} bytes); error-message: {err!r}"
    )

    with zipfile.ZipFile(io.BytesIO(data)) as z:
        names = z.namelist()
        posts = [n for n in names if n.startswith("export/posts/")]
        assert posts, (
            f"the archive lists no posts at all, though one was just composed; "
            f"entries: {sorted(names)[:20]}; error-message: {err!r}"
        )
        bodies = [
            bytes.fromhex(json.loads(z.read(name))["data_hex"]) for name in posts
        ]

    carrier = [name for name, body in zip(posts, bodies) if marker.encode() in body]
    assert carrier, (
        f"the archive carries {len(posts)} post entr{'y' if len(posts) == 1 else 'ies'} "
        f"totalling {sum(len(b) for b in bodies)} bytes, and none of them contains "
        f"the body this journey wrote ({marker!r}). An archive that names the "
        f"user's posts without carrying what they wrote is the index-only "
        f"archive the one-button ruling exists to rule out — the user would have "
        f"exported a catalogue of their own content and none of the content. "
        f"error-message: {err!r}"
    )
