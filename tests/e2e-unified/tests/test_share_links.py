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

import datetime as dt
import json
import secrets
import time
import urllib.error
import urllib.request

import pytest

import fauna_ffi

from conftest import _login_app_as, _make_user, _seed_cross_set_media
from helpers.audience_attestation import attestation_message
from helpers.set_names import audience_update, find_set
from helpers.rpc_hold import refuse_rpc, release_rpc_hold, wait_for_refused_rpc
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
    app, _user, public_set, private_set = _share_link_setup(request, app, nest_instance)
    return app, public_set, private_set


@pytest.fixture
def share_link_owner(request, app, nest_instance):
    """:func:`share_link_app`'s set-up, returning ``(app, user)`` for a journey
    that names the owner to the nest (an aging hook) or ends their identity
    (a succession — dedicated, so no other test's identity is touched)."""
    app, user, _public_set, _private_set = _share_link_setup(request, app, nest_instance)
    return app, user


def _share_link_setup(request, app, nest_instance):
    url = nest_instance["url"]
    port = nest_instance["port"]
    user = _make_user(nest_instance)
    public_set = f"share-pub-{secrets.token_hex(3)}"
    private_set = f"share-priv-{secrets.token_hex(3)}"
    # The private set (the default `private` audience) and its one file —
    # seeded through the SAME device the public file is recorded from, so the
    # seed spends one of the account's device slots, not two. On a live box the
    # dedicated user sits on `free`, whose cap (3 on the dev nest) the second
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
    return app, user, public_set, private_set


def _declassify(url: str, owner: dict, folder: str) -> None:
    """Flip ``folder`` to ``public`` with the owner's genuine attestation."""
    owner_id = bytes(owner["actor_id_bytes"])
    with _actor_client(url, owner) as ws:
        # The set's name rests sealed, so the row is found, and the flip
        # addressed, by the name's hash (`path-sealing.md` § the set-name plane).
        row = find_set(ws.call("fauna.folders.list", {})["folders"], folder)
        assert row is not None, f"the seeded set {folder!r} is listed"
        folder_id = row["id"]
        counter = int(time.time() * 1000)
        sig = owner["signing_key"].sign(
            attestation_message(owner_id, folder_id, folder, counter)
        ).signature
        ws.call(
            "fauna.folders.update",
            audience_update(
                folder,
                "public",
                audience_attestation={
                    "owner": owner_id,
                    "folder_id": folder_id,
                    "counter": counter,
                    "sig": sig,
                },
            ),
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


# ── The create surface and the list, beyond the end-to-end journey ──────────
#
# share-links 3 (four lengths, a week by default, never forever), 5 (each row
# shows its expiry), 8 (Copy gives exactly the link made) and 9 (a link the
# nest did not record is never shown, the page says why, and Create works
# again from the same open surface). The refusal is the nest's `test-hooks`
# rpc-hold refuse on `fauna.share.create`, released in a `finally`. The one
# clock read is today's date, bracketed on both sides of each create so a run
# straddling local midnight still knows which day the app may print. tui leads;
# the other six drop the `tui` mark in their trickle-down.

EXPIRY = "share-link-expiry-select"
CREATE_KIND = "fauna.share.create"
DAY = 24 * 3600
BUDGET_S = 30.0


def _dates_for(secs: int, t0: float, t1: float) -> set[str]:
    """The local dates a link of lifetime ``secs`` made between ``t0`` and
    ``t1`` may print (``fauna_core::format::format_unix_local_date``)."""
    return {
        dt.datetime.fromtimestamp(t + secs).strftime("%Y-%m-%d") for t in (t0, t1)
    }


def _open_create(app) -> None:
    d = app.driver
    m = app.media
    m.navigate()
    wait_until(lambda: FILENAME in m.item_names(), BUDGET_S)
    m.open_item_detail(m.index_of(FILENAME))
    d.wait_for("share-link-button")
    d.click("share-link-button")
    d.wait_for("share-link-create-modal")


def _make_link(app, expiry: str | None) -> tuple[str, float, float]:
    """Make one link to ``FILENAME`` through the create surface, choosing
    ``expiry`` (or leaving the default); return its URL and the time bracket."""
    d = app.driver
    _open_create(app)
    if expiry is not None:
        d.select(EXPIRY, expiry)
        assert d.get_text(EXPIRY) == expiry
    t0 = time.time()
    d.click("share-link-create-button")
    d.wait_for("share-link-url", timeout=BUDGET_S)
    t1 = time.time()
    url = d.get_text("share-link-url")
    d.click("share-link-cancel-button")
    app.media.close_detail()
    return url, t0, t1


@pytest.mark.tui
@pytest.mark.feature("share-links")
def test_four_lengths_a_week_by_default_shown_in_the_list_and_copied_exactly(share_link_app):
    """share-links 3, 5, 8."""
    app, _public_set, _private_set = share_link_app
    d = app.driver

    # ── 3: exactly four lengths, a week preselected, no "never". ──
    _open_create(app)
    assert d.get_text(EXPIRY) == "7d", f"the default is a week: {d.diagnose(EXPIRY)}"
    options = d.option_texts(EXPIRY)
    assert options is not None and len(options) == 4, f"four lengths: {options!r}"
    assert not any("never" in o.lower() for o in options), options
    for value in ("1d", "7d", "30d", "1y"):
        d.select(EXPIRY, value)
        assert d.get_text(EXPIRY) == value
    d.click("share-link-cancel-button")
    app.media.close_detail()

    # Two links of different lengths, so each row's date is its own.
    day_url, d0, d1 = _make_link(app, "1d")
    year_url, y0, y1 = _make_link(app, "1y")
    expected = {
        day_url: _dates_for(DAY, d0, d1),
        year_url: _dates_for(365 * DAY, y0, y1),
    }

    # ── 5 + 8: each row shows its own expiry; its Copy is its own URL. ──
    d.click("share-link-list-button")
    wait_until(lambda: d.count("share-link-item") == 2, BUDGET_S,
               diagnose=lambda: d.diagnose("share-link-list"))
    seen: set[str] = set()
    last = d.get_clipboard_text()
    for i in range(2):
        scope = f"share-link-item[{i}]"
        assert d.get_attr("share-link-item-state", "state", scope=scope) == "active"
        expires = d.get_text("share-link-item-expires", 0, scope=scope)
        d.click("share-link-item-copy-button", 0, scope=scope)
        copied = wait_until(
            lambda: (c := d.get_clipboard_text()) is not None and c != last and c,
            BUDGET_S,
        )
        last = copied
        assert copied in expected, f"row {i} copied {copied!r}, not a link this test made"
        assert any(date in expires for date in expected[copied]), (
            f"row {i} ({copied}) shows {expires!r}; its expiry is one of "
            f"{sorted(expected[copied])}"
        )
        seen.add(copied)
    assert seen == set(expected), "each row copies its own link"


@pytest.mark.tui
@pytest.mark.feature("share-links")
def test_a_link_the_nest_did_not_record_is_never_shown_and_create_tries_again(
    share_link_app, nest_instance
):
    """share-links 9."""
    app, _public_set, _private_set = share_link_app
    d = app.driver
    port = nest_instance["port"]

    _open_create(app)
    refuse_rpc(port, CREATE_KIND)
    try:
        d.click("share-link-create-button")
        wait_for_refused_rpc(port, CREATE_KIND)
        wait_until(
            lambda: d.is_visible("error-message") and d.get_text("error-message").strip(),
            BUDGET_S,
            diagnose=lambda: d.diagnose("error-message"),
        )
        assert d.is_absent("share-link-url"), "a link the nest did not record is never shown"
        assert d.is_absent("share-link-copy-button")
        assert d.is_visible("share-link-create-button"), (
            "the surface stays open, so the user can try again from the same place"
        )
    finally:
        release_rpc_hold(port, CREATE_KIND)

    wait_until(lambda: d.is_enabled("share-link-create-button"),
               BUDGET_S, diagnose=lambda: d.diagnose("share-link-create-button"))
    d.click("share-link-create-button")
    d.wait_for("share-link-url", timeout=BUDGET_S)
    assert "/share/" in d.get_text("share-link-url")


# ── share-links 6: an expired link stays listed, marked expired ─────────────
#
# A link's expiry is signed into it and stored beside its row, and the list
# marks a row `expired` once that time has passed (`fauna_client_share::
# link_state`, read against the wall clock). The shortest length a user can
# choose is a day, so the time cannot pass inside a run. The nest's
# `test-hooks` endpoint `POST /api/v1/test/share_links/age` moves one actor's
# rows back in time and changes nothing else; what the journey then reads is
# the production list.


def _age_share_links(nest_instance, actor_id_hex: str, secs: int) -> int:
    body = json.dumps({"actor_id": actor_id_hex, "secs": secs}).encode()
    req = urllib.request.Request(
        f"{nest_instance['url']}/api/v1/test/share_links/age",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30.0) as resp:
        assert resp.status == 200, f"share-link aging hook returned {resp.status}"
        return json.loads(resp.read())["aged"]


@pytest.mark.tui
@pytest.mark.feature("share-links")
def test_a_link_whose_time_has_run_out_stays_listed_marked_expired(
    share_link_owner, nest_instance
):
    """share-links 6 (`share-links.md` § List): a one-day link, its day gone
    by, is still a row in the list, named from its seal, and its state reads
    `expired` — never dropped from the list."""
    app, user = share_link_owner
    d = app.driver
    _make_link(app, "1d")

    assert _age_share_links(nest_instance, user["actor_id_hex"], 2 * DAY) == 1, (
        "exactly the one link this journey made moved past its expiry"
    )

    d.click("share-link-list-button")
    wait_until(lambda: d.count("share-link-item") == 1, BUDGET_S,
               diagnose=lambda: d.diagnose("share-link-list"))
    scope = "share-link-item[0]"
    assert d.get_text("share-link-item-name", 0, scope=scope) == FILENAME
    assert d.get_attr("share-link-item-state", "state", scope=scope) == "expired", (
        d.diagnose("share-link-item-state")
    )


# ── share-links 16: links survive taking the account back ───────────────────
#
# `succession-repoint-axis.md` § The declared re-point axis: the share-token
# registry is re-pointed to the successor in the succession transaction, and
# after a succession a link serves only through its registry row
# (`share_routes.rs` step 3b). So a link made before the ceremony still
# opens for a stranger, is in the successor's list, and the successor's
# revoke kills it. The ceremony is the one `succeedable_app`'s journeys
# drive; `share_link_owner` is a dedicated actor, so no other test's identity
# is touched.


@pytest.mark.tui
@pytest.mark.feature("share-links")
def test_links_made_before_taking_the_account_back_still_work_and_can_be_revoked(
    share_link_owner,
):
    """share-links 16."""
    from tests.test_identity_succession_aftermath import _run_succession

    app, _user = share_link_owner
    d = app.driver
    url, _t0, _t1 = _make_link(app, None)
    status, body, _ = _fetch(url)
    assert (status, body) == (200, BODY), "the link opens before the ceremony"

    _run_succession(app)

    status, body, _ = _fetch(url)
    assert (status, body) == (200, BODY), (
        f"a link made before taking the account back still opens: {status}"
    )

    app.media.navigate()
    d.click("share-link-list-button")
    wait_until(lambda: d.count("share-link-item") == 1, BUDGET_S,
               diagnose=lambda: f"{d.diagnose('share-link-list')} error={app.error_text()!r}")
    scope = "share-link-item[0]"
    assert d.get_text("share-link-item-name", 0, scope=scope) == FILENAME, (
        "the successor's list names the link from its seal"
    )
    assert d.get_attr("share-link-item-state", "state", scope=scope) == "active"

    d.click("share-link-revoke-button", 0, scope=scope)
    d.wait_for("share-link-revoke-confirm-modal")
    d.click("share-link-revoke-confirm-button")
    wait_until(
        lambda: d.get_attr("share-link-item-state", "state", scope=scope) == "revoked",
        BUDGET_S,
        diagnose=lambda: f"{d.diagnose('share-link-item-state')} error={app.error_text()!r}",
    )
    status, _body, _ = _fetch(url)
    assert status == 410, f"the successor's revoke kills the link: {status}"
