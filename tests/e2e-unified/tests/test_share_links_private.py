"""Private (fragment-keyed) share links — the two journeys. The AUTHOR's:
in the app, a file in an owner-only folder offers a link, the create surface
says the link is the key, and the link it reveals carries the key after `#`
and opens to nothing readable on the nest. The RECIPIENT's, in a real
browser: a person without Fauna opens the link, and the viewer page decrypts
the file in the browser with the shared wasm, checks it, and offers it — while
everything it asks of a server is a same-origin path with no key in it.

Owner doc: ``docs/goal/behavior/share-links.md`` § The private-file extension
(*The viewer is a browser page and needs no account* and its four rules).
Catalog outcomes ``share-links`` 17, 19, 20, 23, 24, 25 and 26. What the nest serves,
observed without a browser, is ``tests/api/test_share_links_private_arm.py``;
the viewer's four rules are also pinned as source contracts in
``apps/fauna-web/src/lib/share-viewer/share-viewer-contract.test.ts``.

Fixture setup (convention 5): ``helpers/private_share.py`` — the owner's sealed
file, recorded as a version for the author's journey (which then makes the link
through the app's own controls, rule 8), and for the recipient's a link made by
the shared ``ShareClient``. What runs as the recipient does is the browser.

The author's journey is tui's — the lead app; the other six inherit the key
notice through the batched trickle-down (share-links.md § Build order). The
recipient's is web-only by nature, marked rather than skipped
(``feature-catalog.md``, the marked-witness rule): the viewer is the one
browser-served surface of the feature, a page no other app hosts.
"""

from __future__ import annotations

import json
import secrets
from pathlib import Path

import pytest

import fauna_ffi

from conftest import _login_app_as, _make_user
from drivers import create_driver
from helpers.private_share import (
    PrivateLink,
    arms,
    get,
    leaks,
    seed_owner_file,
    seed_private_link,
    text_content,
)
from helpers.waiting import wait_until

from tests.api.test_public_folder_fetch import DEVICE_ID
from tests.api.test_web_paywall_folder import _actor_client

pytestmark = pytest.mark.tier_3

FILENAME = "holiday-plan.txt"
AUTHOR_FILENAME = "trip-notes.txt"


def _viewer_state(driver) -> str | None:
    return driver.eval_js(
        "(() => { const r = document.getElementById('share-viewer');"
        " return r ? r.dataset.state : null; })()"
    )


def _viewer_text(driver) -> str:
    return driver.eval_js(
        "(() => { const r = document.getElementById('share-viewer');"
        " return r ? r.innerText : document.body.innerText; })()"
    )


def _storage(driver) -> dict:
    return driver.eval_js(
        "(() => { const o = {}; for (let i = 0; i < localStorage.length; i++) {"
        " const k = localStorage.key(i); o[k] = localStorage.getItem(k); }"
        " return { local: o, session: sessionStorage.length }; })()"
    )


@pytest.fixture
def private_file_app(request, app, nest_instance):
    """``app`` logged in as a DEDICATED actor (so its link list holds only
    this journey's link) owning an owner-only folder whose one file is sealed
    under the owner's root and recorded as a version, as a synced desktop
    writes it. Returns ``(app, content, chunk_count)``."""
    url = nest_instance["url"]
    owner = _make_user(nest_instance)
    vault = f"share-vault-{secrets.token_hex(3)}"
    fauna_ffi.harness_create_set(url, bytes(owner["signing_key"]), {"name": vault})
    with _actor_client(url, owner) as ws:
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )
    content = text_content(400)
    chunk_count = seed_owner_file(
        nest_instance, owner, vault, DEVICE_ID, AUTHOR_FILENAME, content
    )
    _login_app_as(app, request, nest_instance, owner, verify_live_actor=True)
    return app, content, chunk_count


@pytest.mark.tui
@pytest.mark.feature("share-links")
def test_the_author_links_a_private_file_and_the_link_is_the_key(private_file_app):
    app, content, chunk_count = private_file_app
    d = app.driver
    m = app.media
    m.navigate()
    wait_until(lambda: AUTHOR_FILENAME in m.item_names(), 15.0)

    # ── Outcome 17: a file in an owner-only folder offers a link. ──
    m.open_item_detail(m.index_of(AUTHOR_FILENAME))
    d.wait_for("share-link-button")
    d.click("share-link-button")
    d.wait_for("share-link-create-modal")
    # ── Outcome 19: the create surface says the link is the key. ──
    d.wait_for("share-link-key-notice")
    assert d.is_absent("share-link-url"), "no URL before the link is registered"

    d.click("share-link-create-button")
    d.wait_for("share-link-url", timeout=15.0)
    url = d.get_text("share-link-url")
    assert "/share/" in url and "#" in url, url
    assert url.split("#", 1)[1], "the key rides the fragment"

    # ── Outcome 21, from the author's own link: what the nest serves for it —
    # the manifest with its envelope, and every ciphertext chunk — carries no
    # plaintext, no name and no key. (The session nest serves no SPA build, so
    # the viewer page itself is the recipient journey's, below.) ──
    link = PrivateLink(content, AUTHOR_FILENAME, {}, url, chunk_count)
    blind = [(path, *get(path)) for path, navigation in arms(link) if not navigation]
    for path, status, body in blind:
        assert status == 200, (path, status, body[:120])
        assert not leaks(body, content), f"{path} carries plaintext"
        assert AUTHOR_FILENAME.encode() not in body, f"{path} names the file"
        assert link.key.encode() not in body, f"{path} carries the link key"

    d.click("share-link-cancel-button")
    m.close_detail()

    # ── The list names it from its seal, and has no Copy: the key is not on
    # the nest to re-derive the link from (share-links.md § Flows → List). ──
    d.click("share-link-list-button")
    d.wait_for("share-link-item", timeout=15.0)
    assert d.count("share-link-item") == 1
    row = "share-link-item[0]"
    assert d.get_text("share-link-item-name", 0, scope=row) == AUTHOR_FILENAME
    assert d.get_attr("share-link-item-state", "state", scope=row) == "active"
    assert d.is_absent("share-link-item-copy-button", scope=row)

    # ── Outcome 22: a revoke stops the nest serving every part of it. ──
    d.click("share-link-revoke-button", 0, scope=row)
    d.wait_for("share-link-revoke-confirm-modal")
    d.click("share-link-revoke-confirm-button")
    wait_until(
        lambda: d.get_attr("share-link-item-state", "state", scope=row) == "revoked",
        15.0,
    )
    for path, navigation in arms(link):
        if not navigation:
            status, _body = get(path)
            assert status == 410, (path, status)


@pytest.mark.web
@pytest.mark.feature("share-links")
def test_a_stranger_opens_a_private_link_in_a_browser(share_viewer_nest):
    nest = share_viewer_nest
    owner = _make_user(nest)
    link = seed_private_link(nest, owner, text_content(), FILENAME)

    driver = create_driver("web")
    # Land first on the link's origin — the viewer entry itself under `/app/`,
    # which is the generic page — and put a signed-in owner's identity where
    # the app keeps it: outcome 25 is that the viewer reads and writes none of
    # it. A different path from the link's, so opening the link below is a
    # real page load, not a same-document fragment change.
    origin = nest["url"].rstrip("/")
    driver.launch({"url": f"{origin}/app/share-viewer.html"})
    try:
        wait_until(lambda: _viewer_state(driver) == "generic", 30.0)
        driver.eval_js(
            "localStorage.setItem('fauna_secret', "
            f"{json.dumps(bytes(owner['signing_key']).hex())}); true"
        )
        before = _storage(driver)

        # ── Outcome 20: the whole link opens to the file, checked. ──
        driver._post("/navigate", {"url": link.url})
        wait_until(
            lambda: _viewer_state(driver) in ("ready", "failed"),
            60.0,
            diagnose=lambda: _viewer_text(driver),
        )
        assert _viewer_state(driver) == "ready", _viewer_text(driver)
        assert (
            driver.eval_js("document.querySelector('#share-viewer h2').textContent")
            == FILENAME
        )
        driver.eval_js("document.querySelector('#share-viewer a.download').click(); true")

        def downloaded() -> bytes | None:
            target = Path(driver.download_dir() or "") / FILENAME
            return target.read_bytes() if target.is_file() else None

        got = wait_until(downloaded, 30.0)
        assert got == link.content, "the download is not the linked file"

        # ── Outcome 23: plain text shows in place, as text, and nothing is
        # rendered as a document. ──
        assert (
            driver.eval_js("document.querySelector('#share-viewer pre').textContent")
            == link.content.decode("ascii")
        )
        assert driver.eval_js(
            "document.querySelectorAll('iframe, object, embed, frame').length"
        ) == 0

        # ── Outcome 24: every request the page made is a same-origin path,
        # none carrying the key. ──
        requested = driver.eval_js(
            "performance.getEntriesByType('resource').map((e) => e.name)"
        )
        origin = driver.eval_js("location.origin")
        assert any("/manifest" in u for u in requested), requested
        for u in requested:
            assert u.startswith(origin + "/"), f"the viewer reached another origin: {u}"
            assert link.key not in u and "#" not in u, f"a request carried the key: {u}"

        # ── Outcome 26: the address bar still holds the whole link. ──
        assert driver.eval_js("location.href") == link.url
        # ── Outcome 25: no identity slot or account state was touched. ──
        assert _storage(driver) == before
    finally:
        driver.teardown()
