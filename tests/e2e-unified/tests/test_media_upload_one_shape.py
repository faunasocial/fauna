"""A file uploaded through the Media page is a folder file like any other —
the two journeys the one at-rest shape exists for.

Owner doc: ``docs/goal/ui/media.md`` § Encryption at rest → *One at-rest shape
for a folder's file* (ruled 2026-10-06). Every file the Media page records into
a folder rests as the sync engine's shape — chunks behind a ``ChunkManifest``,
sealed under the folder's custody root (or plaintext for a ``public`` folder) —
so every reader of that shape opens it with no second arm. Until the producer
moved, a Media upload rested as ONE blob-store primary and the two readers that
were never taught it failed exactly where a user looks:

1. **A private link** (``share-links.md`` § Which files can be linked): a Media
   upload into an owner-only folder was offered ``share-link-button`` and the
   create failed — the blob had no per-chunk keys to hand out. Here the author
   uploads through the page, makes the link through the page, and a stranger
   holding nothing but the link opens the bytes through the viewer's own shared
   open + assemble (``fauna_client_share::viewer``, the functions the viewer
   page's wasm runs), over the same GETs its browser would make. The browser
   rendering of that page is ``test_share_links_private.py``'s recipient
   journey; this one is the path from the Media page to it.
2. **A website** (``web-content-hosting.md`` § Content model): a Media upload
   into a public website folder answered 404 at the site — the serve walk reads
   only a manifest. Here the owner publishes the folder through the folders
   page and drops the page in through Media, and ``GET /`` at their address
   answers with its bytes. ``test_public_website_folder_serve.py`` is the same
   site reached through a bound location and the sync agent.

Both journeys drive every mutation through the app (convention 8). tui is the
lead app; the other six inherit both witnesses through the batched trickle-down
(the gesture is the shared ``MediaMachine::upload_selected``, so a mark is what
each adds).
"""

from __future__ import annotations

import secrets

import pytest

import fauna_ffi

from conftest import MAIL_PRIMARY_DOMAIN
from helpers.private_share import get, leaks, text_content
from helpers.waiting import wait_until

from tests.test_public_website_folder_serve import (
    _SERVE_WINDOW_SECS,
    _await_folder_flags,
    _get_root_with_host,
)

pytestmark = pytest.mark.tier_3


@pytest.mark.tui
@pytest.mark.feature("share-links")
def test_a_media_upload_into_a_private_folder_takes_a_link_a_stranger_opens(
    empty_media_app, tmp_path
):
    app = empty_media_app
    d = app.driver
    m = app.media
    vault = f"vault-{secrets.token_hex(3)}"
    filename = f"plan-{secrets.token_hex(3)}.txt"
    content = text_content(2000)

    # The folder is born owner-only — the wizard's default, nothing to choose.
    app.backups.navigate_folders()
    app.backups.create_folder_via_wizard(vault)

    # ── The upload, through the Media page into its only folder. ──
    m.navigate()
    m.ensure_loaded()
    picked = tmp_path / filename
    picked.write_bytes(content)
    m.upload_file(str(picked))
    assert m.wait_for_item_count(1) == 1, (
        f"the upload should produce one item; error={app.error_text()!r}"
    )
    assert not app.has_error(), f"upload error: {app.error_text()!r}"

    # ── The link, through the page's own create control. ──
    m.open_item_detail(m.index_of(filename))
    d.wait_for("share-link-button")
    d.click("share-link-button")
    d.wait_for("share-link-create-modal")
    d.wait_for("share-link-key-notice")
    d.click("share-link-create-button")

    def _revealed():
        if d.is_visible("share-link-url"):
            return d.get_text("share-link-url")
        return None

    url = wait_until(
        _revealed,
        15.0,
        diagnose=lambda: (
            "the create never revealed a link — before the one at-rest shape this "
            "is exactly where a Media upload failed (a blob primary has no chunk "
            f"keys to hand out); error={app.error_text()!r}"
        ),
    )
    assert "/share/" in url and "#" in url, url

    # ── The stranger: nothing but the link, and the nest's answers to it. ──
    def fetch(path_url: str) -> bytes:
        status, body = get(path_url)
        assert status == 200, (path_url, status, body[:200])
        assert not leaks(body, content), f"{path_url} carries plaintext"
        assert filename.encode() not in body, f"{path_url} names the file"
        return body

    opened_name, opened = fauna_ffi.share_viewer_open(url, fetch)
    assert opened_name == filename
    assert opened == content, "the link's holder must recover the uploaded bytes"

    d.click("share-link-cancel-button")
    m.close_detail()


@pytest.mark.parametrize("folder_share_owner_app", ["tui"], indirect=True)
@pytest.mark.real_conversations
@pytest.mark.timeout(900)
@pytest.mark.feature("public-folders-and-websites")
def test_a_media_upload_into_a_public_website_folder_is_served_at_the_owners_address(
    folder_share_owner_app, tmp_path
):
    app, nest, owner = folder_share_owner_app
    nest_url = nest["url"]
    set_name = f"site-{secrets.token_hex(4)}"
    marker = f"media-site-{secrets.token_hex(6)}"
    page = f"<!doctype html><title>{marker}</title><h1>{marker}</h1>\n"

    # ── Publish the folder: create, make public, serve as the website, and opt
    # the address in — the same three switches the agent-driven proof walks. ──
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(set_name)
    b.make_public(set_name)
    _await_folder_flags(nest_url, owner, set_name, audience="public")
    if not b.website_toggle_visible():
        b.find_and_expand_folder(set_name)
    b.toggle_website()
    _await_folder_flags(
        nest_url, owner, set_name, audience="public", website_enabled=True
    )
    app.web_settings.navigate()
    app.web_settings.set_subdomain_enabled(True)

    # ── Drop the page in through Media, into that folder. ──
    m = app.media
    m.navigate()
    m.ensure_loaded()
    m.set_filter(set_name)
    picked = tmp_path / "index.html"
    picked.write_text(page)
    m.upload_file(str(picked))
    assert m.wait_for_item_count(1) == 1, (
        f"the upload should produce one item; error={app.error_text()!r}"
    )
    assert not app.has_error(), f"upload error: {app.error_text()!r}"

    # ── The site serves those bytes. ──
    fqdn = f"{owner['handle']}.{MAIL_PRIMARY_DOMAIN}"
    seen: dict[str, object] = {"status": None, "body": ""}

    def _serves_our_bytes():
        status, served = _get_root_with_host(nest_url, fqdn)
        seen["status"], seen["body"] = status, served
        return status == 200 and marker in served

    wait_until(
        _serves_our_bytes,
        _SERVE_WINDOW_SECS,
        interval=2.0,
        diagnose=lambda: (
            f"GET / on {fqdn!r} did not serve the page uploaded through Media.\n"
            f"  status={seen['status']}\n"
            f"  body[:400]={str(seen['body'])[:400]!r}\n"
            "  A 404 means the serve walk could not read what Media recorded — the "
            "blob-primary shape the one at-rest shape retired; an INFO PAGE means "
            "the subdomain opt-in did not round-trip."
        ),
    )
