"""Browser-openable share links — the author's journey, end to end on a real
binary and a real nest: make a link to a file in a public-audience folder,
open it as a stranger over plain HTTP, see it in the list, revoke it, and
watch the link die.

Owner doc: ``docs/goal/behavior/share-links.md`` (§ Which files can be linked,
§ Flows → Create / List / Revoke, § Build order's journey). The route and its
``410`` are ``docs/goal/architecture/core-client-kind-catalog.md`` § Share.

What runs through the UI (rule 8): the create, the list, the copy, the revoke.
What is fixture setup (the ``test_folder_follow_media_browse`` API carve-out):
the owner's two folders — one declassified to ``public`` holding one real
plaintext file, one private — and the stranger's anonymous fetch, which is the
thing a person without Fauna does in a browser.

tui led (`docs/goal/behavior/share-links.md` § Build order); the other six
followed in the batched trickle-down, and all seven apps run it.
"""

import secrets
import time
import urllib.error
import urllib.request

import pytest

import fauna_ffi

from conftest import _login_app_as, _make_user, _seed_cross_set_media
from helpers.audience_attestation import attestation_message
from helpers.waiting import wait_until

from tests.api.test_public_folder_fetch import DEVICE_ID, _record
from tests.api.test_web_paywall_folder import _actor_client

pytestmark = pytest.mark.tier_3

BODY = b"a holiday photo, as far as this test is concerned\n" * 8
FILENAME = "holiday.txt"


@pytest.fixture
def share_link_app(request, app, nest_instance):
    """``app`` logged in as a DEDICATED actor owning a public set with one real
    file and a private set with one file. Dedicated so the list this journey
    reads holds exactly its own links. Returns ``(app, public_set, private_set)``.
    """
    url = nest_instance["url"]
    port = nest_instance["port"]
    user = _make_user(nest_instance)
    public_set = f"share-pub-{secrets.token_hex(3)}"
    private_set = f"share-priv-{secrets.token_hex(3)}"
    # The private set (the default `private` audience) and its one file —
    # seeded through the SAME device the public file is recorded from, so the
    # seed spends one of the account's device slots, not two. On a live box the
    # dedicated user sits on `free`, whose cap (3 on dev.example.com) the second
    # seed device filled before the app's own machine registered: the app was
    # refused
    # (`device limit reached for your tier`), its account runtime torn down,
    # no folder nonce, and both rows held out of the listing.
    _seed_cross_set_media(
        nest_instance, user, {private_set: ["diary.txt"]}, device_id=DEVICE_ID.hex()
    )
    with _actor_client(url, user) as ws:
        fauna_ffi.harness_create_set(
            url, bytes(user["signing_key"]),
            {"name": public_set},
        )
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )
    # The public flip carries the OWNER's signed attestation, exactly as the
    # app's `folder-audience-public-confirm` sends it: a seat trusts a
    # declassification only when the owner attested it, so a bare
    # `audience: public` is — correctly — never eligible for a link
    # (`encryption-at-rest.md` § Readable classes, the owner-attested rule).
    _declassify(url, user, public_set)
    # The public file is recorded while the set is public (its bytes rest in
    # the clear, which is what the public arm serves).
    _record(url, port, user, public_set, FILENAME, BODY)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    return app, public_set, private_set


def _declassify(url: str, owner: dict, folder: str) -> None:
    """Flip ``folder`` to ``public`` with the owner's genuine attestation."""
    owner_id = bytes(owner["actor_id_bytes"])
    with _actor_client(url, owner) as ws:
        rows = ws.call("fauna.folders.list", {})
        folder_id = next(f["id"] for f in rows["folders"] if f["name"] == folder)
        counter = int(time.time() * 1000)
        sig = owner["signing_key"].sign(
            attestation_message(owner_id, folder_id, folder, counter)
        ).signature
        ws.call(
            "fauna.folders.update",
            {
                "name": folder,
                "audience": "public",
                "audience_attestation": {
                    "owner": owner_id,
                    "folder_id": folder_id,
                    "counter": counter,
                    "sig": sig,
                },
            },
        )


def _fetch(url: str) -> tuple[int, bytes, str]:
    """GET ``url`` as a stranger: no identity header, no cookie."""
    try:
        with urllib.request.urlopen(url, timeout=15) as resp:
            return resp.status, resp.read(), resp.headers.get("Content-Disposition", "")
    except urllib.error.HTTPError as err:
        return err.code, err.read(), err.headers.get("Content-Disposition", "")


@pytest.mark.feature("share-links")
def test_create_open_list_and_revoke_a_share_link(share_link_app):
    app, public_set, private_set = share_link_app
    d = app.driver

    m = app.media
    m.navigate()
    wait_until(lambda: FILENAME in m.item_names() and "diary.txt" in m.item_names(), 15.0)

    # ── Eligibility: offered on the public file AND on the private one — an
    #    owner-only folder's file takes the fragment-keyed link (catalog
    #    outcome 17; its own journey is test_share_links_private.py). ──
    m.open_item_detail(m.index_of("diary.txt"))
    d.wait_for("share-link-button")
    m.close_detail()

    m.open_item_detail(m.index_of(FILENAME))
    d.wait_for("share-link-button")

    # ── Create with the default expiry; the URL only after registration. ──
    d.click("share-link-button")
    d.wait_for("share-link-create-modal")
    assert d.is_absent("share-link-url"), "no URL before the link is registered"
    d.click("share-link-create-button")
    d.wait_for("share-link-url", timeout=15.0)
    url = d.get_text("share-link-url")
    assert "/share/" in url, url

    # ── A stranger opens it over plain HTTP. ──
    status, body, disposition = _fetch(url)
    assert status == 200, (status, body[:200])
    assert body == BODY
    assert FILENAME in disposition, disposition

    d.click("share-link-cancel-button")
    m.close_detail()

    # ── The list: one Active row, named from its seal, whose Copy is the URL. ──
    d.click("share-link-list-button")
    d.wait_for("share-link-item", timeout=15.0)
    assert d.count("share-link-item") == 1
    assert d.get_text("share-link-item-name", 0, scope="share-link-item[0]") == FILENAME
    assert d.get_attr("share-link-item-state", "state", scope="share-link-item[0]") == "active"
    d.click("share-link-item-copy-button", 0, scope="share-link-item[0]")
    copied = d.get_clipboard_text()
    if copied is not None:
        assert copied == url

    # ── Revoke, through its single confirm; the link now answers 410. ──
    d.click("share-link-revoke-button", 0, scope="share-link-item[0]")
    d.wait_for("share-link-revoke-confirm-modal")
    d.click("share-link-revoke-confirm-button")
    wait_until(
        lambda: d.get_attr("share-link-item-state", "state", scope="share-link-item[0]")
        == "revoked",
        15.0,
    )
    assert d.is_absent("share-link-item-copy-button", scope="share-link-item[0]")
    assert d.is_absent("share-link-revoke-button", scope="share-link-item[0]")
    status, _body, _ = _fetch(url)
    assert status == 410, status
