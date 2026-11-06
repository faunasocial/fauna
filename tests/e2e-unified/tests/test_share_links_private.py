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
file, recorded as a version. In both journeys the column's own app then makes
the link through its own controls (rule 8); what runs as the recipient does is
an ordinary browser, the same one whichever app made the link.

The author's journey is tui's — the lead app; linux, web, android and windows
inherited the key notice through the batched trickle-down (share-links.md §
Build order). The recipient's runs on every column whose app makes the link:
the viewer is the one browser-served surface of the feature, so each column's
arm is "this app made it, a browser opened it" — the nest serving the viewer
(``share_viewer_nest``) is the web build, which only Linux builds, so a column
off Linux reads an environment skip. tui and linux lead; windows, macos, ios
and android follow in their batched legs.
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


def _owner_with_a_private_file(request, app, nest, filename, content, **login):
    """Log ``app`` in on ``nest`` as a DEDICATED actor (so its link list holds
    only this journey's link) owning an owner-only folder whose one file,
    ``content`` named ``filename``, is sealed under the owner's root and
    recorded as a version, as a synced desktop writes it. Returns the chunk
    count."""
    url = nest["url"]
    owner = _make_user(nest)
    vault = f"share-vault-{secrets.token_hex(3)}"
    fauna_ffi.harness_create_set(url, bytes(owner["signing_key"]), {"name": vault})
    with _actor_client(url, owner) as ws:
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )
    chunk_count = seed_owner_file(nest, owner, vault, DEVICE_ID, filename, content)
    _login_app_as(app, request, nest, owner, verify_live_actor=True, **login)
    return owner, chunk_count


def _create_link_in_app(app, filename: str) -> str:
    """Make a link to ``filename`` through the app's own controls (rule 8):
    the file's detail, Share a link, the key notice, Create. Returns the URL
    the app reveals, leaving the create surface open."""
    d = app.driver
    m = app.media
    m.navigate()
    wait_until(lambda: filename in m.item_names(), 15.0,
               diagnose=lambda: f"items={m.item_names()!r} error={app.error_text()!r}")
    m.open_item_detail(m.index_of(filename))
    d.wait_for("share-link-button")
    d.click("share-link-button")
    d.wait_for("share-link-create-modal")
    d.wait_for("share-link-key-notice")
    assert d.is_absent("share-link-url"), "no URL before the link is registered"
    d.click("share-link-create-button")
    d.wait_for("share-link-url", timeout=15.0)
    url = d.get_text("share-link-url")
    assert "/share/" in url and "#" in url, url
    assert url.split("#", 1)[1], "the key rides the fragment"
    return url


@pytest.fixture
def private_file_app(request, app, nest_instance):
    """``app`` on the session nest, owning one private file. Returns
    ``(app, content, chunk_count)``."""
    content = text_content(400)
    _owner, chunk_count = _owner_with_a_private_file(
        request, app, nest_instance, AUTHOR_FILENAME, content
    )
    return app, content, chunk_count


@pytest.fixture
def share_viewer_spa_url(static_dir, share_viewer_nest):
    """Function-scoped SPA proxy to ``share_viewer_nest`` for the web column's
    app; requested lazily, so a native column never asks for it."""
    from conftest import _serve_spa_proxy
    url, server = _serve_spa_proxy(static_dir, share_viewer_nest["url"])
    yield url
    server.shutdown()


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("share-links")
def test_the_author_links_a_private_file_and_the_link_is_the_key(private_file_app):
    app, content, chunk_count = private_file_app
    d = app.driver
    m = app.media

    # ── Outcome 17: a file in an owner-only folder offers a link; outcome 19:
    # the create surface says the link is the key. ──
    url = _create_link_in_app(app, AUTHOR_FILENAME)

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


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("share-links")
def test_a_stranger_opens_a_private_link_in_a_browser(request, app, share_viewer_nest):
    nest = share_viewer_nest
    content = text_content()
    owner, chunk_count = _owner_with_a_private_file(
        request, app, nest, FILENAME, content, spa_url_fixture="share_viewer_spa_url"
    )
    # The column's own app makes the link; the browser below is the stranger's.
    link = PrivateLink(content, FILENAME, {}, _create_link_in_app(app, FILENAME), chunk_count)
    app.driver.click("share-link-cancel-button")
    app.media.close_detail()

    driver = create_driver("web")
    # Land first on the link's origin — the viewer entry itself under `/app/`,
    # which is the generic page — and put a signed-in owner's identity where
    # the app keeps it: outcome 25 is that the viewer reads and writes none of
    # it. A different path from the link's, so opening the link below is a
    # real page load, not a same-document fragment change.
    # The link's origin is the one its app made it on — the nest, or on web the
    # SPA proxy standing in for the nest's own origin.
    origin = link.path_url.split("/share/", 1)[0]
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


# ── share-links 18: a folder shared with named people offers no link ───────
#
# `share-links.md` § The private-file extension, *Eligibility*: a bound
# (`shared`-audience) folder's file is not linkable — a co-authored scope must
# not be opened by one member's gesture — unless the owner makes the folder
# public. The predicate is `fauna_client_share::share_link_eligible` over the
# Media machine's folder facts; this walks the chain a person does. The file is
# fixture setup (sealed under the owner's root while the folder is still
# owner-only, so the first look is the vacuity guard: it DID offer a link); the
# share and the public flip are the app's own gestures (rule 8). The recipient
# is headless — bound-ness is minted by the owner's share and waits on nobody
# accepting (`test_folder_bound_flip_back.py`'s idiom). tui leads; linux joins
# here, then macos, ios, web and windows; the other columns follow with
# `folder_share_owner_app`'s params.

SHARED_FILENAME = "family-budget.txt"
_ROSTER_S = 60.0
_UI_S = 30.0


def _offers_a_link(app, filename: str) -> bool:
    """A fresh visit to ``filename``'s detail: whether it offers Share a link.
    Fresh each time, so a page that does not poll still reads the folder's
    current facts."""
    m = app.media
    m.navigate()
    wait_until(lambda: filename in m.item_names(), _UI_S,
               diagnose=lambda: f"items={m.item_names()!r} error={app.error_text()!r}")
    m.open_item_detail(m.index_of(filename))
    try:
        return not app.driver.is_absent("share-link-button")
    finally:
        m.close_detail()


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.parametrize(
    "folder_share_owner_app", ["tui", "linux", "macos", "ios", "web", "windows", "android"], indirect=True
)
@pytest.mark.real_conversations
@pytest.mark.feature("share-links")
def test_a_folder_shared_with_named_people_offers_no_link_unless_public(
    folder_share_owner_app,
):
    """share-links 18."""
    from helpers.admin_wire import admit_adult
    from conftest import MAIL_PRIMARY_DOMAIN
    from tests.api import conv_api

    app, nest, owner = folder_share_owner_app
    b = app.backups

    recipient_handle = "linkbob" + secrets.token_hex(2)
    recipient = admit_adult(nest, recipient_handle, domain=MAIL_PRIMARY_DOMAIN, token=True)
    conv_api.keypackage_upload(
        nest["port"],
        recipient,
        conv_api.mint_key_packages(bytes(recipient["signing_key"]), 2),
    )

    name = f"share-household-{secrets.token_hex(3)}"
    fauna_ffi.harness_create_set(nest["url"], bytes(owner["signing_key"]), {"name": name})
    with _actor_client(nest["url"], owner) as ws:
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )
    seed_owner_file(nest, owner, name, DEVICE_ID, SHARED_FILENAME, text_content(40))

    # ── The vacuity guard: while owner-only, the file DOES offer a link. ──
    assert _offers_a_link(app, SHARED_FILENAME), (
        "an owner-only folder's file must offer a link before the share, or "
        f"the absence below proves nothing: {app.driver.diagnose('share-link-button')}"
    )

    # ── Share the folder with a named person — the gesture that binds it. ──
    b.navigate_folders()
    b.find_and_expand_folder(name)
    b.open_share_dialog()
    b.share_recipient(handle=recipient_handle, actor_id_hex=recipient["actor_id_hex"])
    wait_until(
        lambda: b.shared_member_count() == 1,
        _ROSTER_S,
        diagnose=lambda: (
            "the share never landed a roster member, so the folder never bound; "
            f"nest roster={conv_api.folder_member_actors(nest['port'], owner, name)!r} "
            f"error={app.error_text()!r}"
        ),
    )

    # ── 18: the shared folder's file offers no link. ──
    wait_until(
        lambda: not _offers_a_link(app, SHARED_FILENAME),
        _UI_S,
        diagnose=lambda: f"still offered on a shared folder; error={app.error_text()!r}",
    )

    # ── … unless the folder is public. ──
    b.navigate_folders()
    if not b.audience_select_visible():
        b.find_and_expand_folder(name)
    wait_until(
        b.audience_select_visible,
        _UI_S,
        diagnose=lambda: app.driver.diagnose("folder-audience-select"),
    )
    b.set_audience("public")
    wait_until(
        b.declassify_confirm_visible,
        _UI_S,
        diagnose=lambda: f"no public confirm armed; error={app.error_text()!r}",
    )
    b.confirm_public()
    # The confirm is modal: a person cannot leave the page until it closes, and
    # the apple seam holds it up until the write lands. Navigating over it
    # tore its host down mid-dismissal and crashed the macOS app.
    wait_until(
        lambda: not b.declassify_confirm_visible(),
        _UI_S,
        diagnose=lambda: f"the public confirm never closed; error={app.error_text()!r}",
    )
    wait_until(
        lambda: _offers_a_link(app, SHARED_FILENAME),
        _UI_S,
        diagnose=lambda: f"a public folder's file offers no link; error={app.error_text()!r}",
    )
