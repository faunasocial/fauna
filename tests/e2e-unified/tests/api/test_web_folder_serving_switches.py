"""The switches around a public website folder — the enable-time backfill, the
cross-toggle refusals, and the file a nest will never serve — on a real
``fauna-nest`` binary.

Sibling of ``test_web_folder_audience.py``, which pins the audience gate itself.
This file pins the three things that gate leaves open:

* **The backfill** (``web-content-hosting.md`` § Content model — the ``web_files``
  projection, rebuilt by ``reconcile_web_files_projection`` when a folder's
  serving transition lands in an enabled state). Both existing serve tests drop
  the file in *after* the switches, so the ordering a real user hits — files
  already synced, website switch flipped later — was the one nothing covered.
* **The cross-toggle refusals** (``ui/folders.md`` § Audience and website serving:
  "Each pair is refused whichever side moves second, and the refusal text names
  the repair"). Two pairs, two orders each.
* **The executable refusal** (§ Content model: files with server-side-execution
  extensions "are rejected at sync time"), at **both** doors a file can arrive
  through — the sync ingest and the enable-time backfill.

The refusal texts are read off ``RpcCallError.details``, the free-form blob the
nest attaches beside the typed code: the code says *that* it refused, the detail
is where the repair is named.
"""

import pytest

import fauna_ffi

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register

from tests.api.test_web_folder_audience import DEVICE_ID, _seed_public_file
from tests.api.test_web_paywall_folder import _actor_client, _get

pytestmark = pytest.mark.tier_3

# The device the imported `_seed_public_file` records its changes as — the
# helper and its device id travel together, so registering any other one
# leaves the seed refused with `fauna.sync.device_unregistered`.
MARKER = "folder-switch-marker"
BODY = f"<h1>Already here</h1>\n<p>{MARKER}</p>\n".encode()
EXEC_MARKER = "never-served-executable-marker"
EXEC_BODY = f"<?php echo '{EXEC_MARKER}'; ?>\n".encode()


def _register_device(ws) -> None:
    ws.call(
        "fauna.sync.register",
        {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
    )


def _serving_at_apex(nest, actor):
    """Context manager: serve `actor`'s site at the apex, then clean up.

    The shared session nest has no domain, so the apex catch-all answers every
    host — the same seam `test_web_folder_audience` and `test_web_paywall_folder`
    use. The apex actor is a nest-wide singleton, so it is always given back.
    """
    import contextlib

    @contextlib.contextmanager
    def _ctx():
        admin_sk = nest["admin"]["signing_key"]
        admin_ws = WsRpcAdminClient(
            nest["url"], actor_id=bytes(admin_sk.verify_key), signing_key=bytes(admin_sk)
        )
        with admin_ws:
            admin_ws.call(
                "fauna.web.set_apex_actor", {"actor_id": bytes(actor["actor_id_bytes"])}
            )
        try:
            yield
        finally:
            with admin_ws:
                admin_ws.call("fauna.web.set_apex_actor", {"actor_id": None})

    return _ctx()


@pytest.mark.feature("public-folders-and-websites")
def test_turning_the_website_switch_on_serves_the_files_already_there(nest_instance):
    """A public folder whose files synced BEFORE the website switch was flipped
    serves those files, not an empty site.

    The ordering is the point. A folder that is `public` but website-OFF is not
    a render input and gets no `web_files` row at ingest time
    (`route_web_file_change` keys on the toggle), so the bytes sitting in the
    folder are invisible to the serve walk until something projects them. What
    projects them is the enable-time backfill in `fauna.folders.update` — and
    without it the user's first visit to their brand-new site is a 404 against
    files they can see in Media.
    """
    url, port = nest_instance["url"], nest_instance["port"]
    creator = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    site = "already-synced-site"
    page = "index.html"

    # ── 1. Public folder, website switch OFF, and a file synced into it. ──
    with _actor_client(url, creator) as ws:
        fauna_ffi.harness_create_set(
            url, bytes(creator["signing_key"]),
            {"name": site, "audience": "public"},
        )
        _register_device(ws)
        row = next(f for f in ws.call("fauna.folders.list", {})["folders"] if f["name"] == site)
        assert row["website_enabled"] is False, (
            f"the switch must start OFF or the backfill is not what serves: {row}"
        )

    _seed_public_file(url, port, creator, site, page, BODY)

    with _serving_at_apex(nest_instance, creator):
        # Nothing serves yet — the corpus is there, the switch is not.
        status, body, _headers = _get(url, f"/{page}")
        assert status != 200 or MARKER not in body, (
            f"a website-OFF folder must not serve: {status} {body}"
        )

        # ── 2. The switch, and only the switch. No re-sync, no re-upload. ──
        with _actor_client(url, creator) as ws:
            ws.call("fauna.folders.update", {"name": site, "website_enabled": True})

        status, body, _headers = _get(url, f"/{page}")
        assert status == 200 and MARKER in body, (
            "turning the website switch on must serve the files already in the "
            f"folder, not an empty site: {status} {body}"
        )


@pytest.mark.feature("public-folders-and-websites")
def test_an_executable_file_is_never_served_however_it_got_into_the_folder(nest_instance):
    """A file the nest could be made to run never serves — neither when it syncs
    into a folder already serving, nor when it is swept up by the enable-time
    backfill.

    Two doors, one rule (`web_content::serve::rejected_extension`, asked by
    `route_web_file_change` at ingest and by `reconcile_web_files_projection` at
    the backfill). Covering only the first door is the shape of the bug the
    backfill could have introduced: a `.php` that arrived while the site was off
    would have been projected into `web_files` by the reconcile that turned it
    on.

    An ordinary file rides beside the executable through each door, so a run
    where nothing serves at all cannot pass as a refusal.
    """
    url, port = nest_instance["url"], nest_instance["port"]
    creator = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    live_site = "exec-live-site"
    later_site = "exec-backfill-site"

    with _actor_client(url, creator) as ws:
        # Door 1's folder: serving from the start.
        fauna_ffi.harness_create_set(
            url, bytes(creator["signing_key"]),
            {"name": live_site, "audience": "public"},
        )
        ws.call("fauna.folders.update", {"name": live_site, "website_enabled": True})
        # Door 2's folder: public, but not serving yet.
        fauna_ffi.harness_create_set(
            url, bytes(creator["signing_key"]),
            {"name": later_site, "audience": "public"},
        )
        _register_device(ws)

    _seed_public_file(url, port, creator, live_site, "live.html", BODY)
    _seed_public_file(url, port, creator, live_site, "live-hook.php", EXEC_BODY)
    _seed_public_file(url, port, creator, later_site, "later.html", BODY)
    _seed_public_file(url, port, creator, later_site, "later-hook.php", EXEC_BODY)

    with _serving_at_apex(nest_instance, creator):
        # ── Door 1 — sync into a folder that is already serving. ──
        status, body, _headers = _get(url, "/live.html")
        assert status == 200 and MARKER in body, (
            f"the control file must serve, or the refusal proves nothing: {status} {body}"
        )
        status, body, _headers = _get(url, "/live-hook.php")
        assert status != 200 and EXEC_MARKER not in body, (
            f"an executable file served from a live site: {status} {body}"
        )

        # ── Door 2 — swept up by the enable-time backfill. ──
        with _actor_client(url, creator) as ws:
            ws.call("fauna.folders.update", {"name": later_site, "website_enabled": True})

        status, body, _headers = _get(url, "/later.html")
        assert status == 200 and MARKER in body, (
            f"the backfill must project the ordinary file: {status} {body}"
        )
        status, body, _headers = _get(url, "/later-hook.php")
        assert status != 200 and EXEC_MARKER not in body, (
            f"the enable-time backfill projected an executable file: {status} {body}"
        )


TIER = "supporters"


def _refusal(ws, kind: str, params: dict) -> RpcCallError:
    """Call `kind` expecting a refusal; return the error for its detail text."""
    with pytest.raises(RpcCallError) as exc:
        ws.call(kind, params)
    assert exc.value.code == "fauna.folders.invalid_request", exc.value.code
    return exc.value


def _detail(err: RpcCallError) -> str:
    assert isinstance(err.details, str), (
        f"the refusal must carry its detail text, got {err.details!r}"
    )
    return err.details


@pytest.mark.feature("public-folders-and-websites")
def test_public_refuses_webdav_and_a_paywall_whichever_moves_second(nest_instance):
    """A public folder refuses to also be served to standard file apps or put
    behind a paywall — in either order — and each refusal names the repair.

    Two incompatible pairs, and the nest must refuse whichever side moves
    second, because a user reaches the same illegal state from both directions:

    * ``public`` ⊕ WebDAV serving — DAV serving is content-key-sealed and a
      public folder rests unsealed, so there is no key to convey.
    * ``public`` ⊕ a paywall — a world-readable paywall is no paywall.

    Four calls, four refusals, four repairs named (``ui/folders.md``
    § Audience and website serving).
    """
    url, port = nest_instance["url"], nest_instance["port"]
    creator = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )

    with _actor_client(url, creator) as ws:
        ws.call(
            "fauna.subscriptions.tiers.create",
            {
                "name": TIER,
                "rank": 1,
                "description": None,
                "price_hint": "3 EUR / month",
                "payment_url": "https://pay.example/supporters",
                "auto_approve": False,
                # The required birth KeyBlob (empty roster).
                "encrypted_upload": fauna_ffi.build_tier_birth_upload(bytes(creator["signing_key"]), TIER, b"\x5a" * 32),
            },
        )

        # ── Pair 1, order A: public first, then WebDAV. ──
        ws.call(
            "fauna.folders.create",
            {"name": "pub-then-dav",
             "audience": "public"},
        )
        detail = _detail(
            _refusal(ws, "fauna.folders.update",
                     {"name": "pub-then-dav", "webdav_enabled": True})
        )
        assert "WebDAV" in detail, detail
        assert "private or shared" in detail, (
            f"the refusal must name the repair, not only the conflict: {detail}"
        )

        # ── Pair 1, order B: WebDAV first, then public. ──
        ws.call(
            "fauna.folders.create",
            {"name": "dav-then-pub"},
        )
        ws.call("fauna.folders.update", {"name": "dav-then-pub", "webdav_enabled": True})
        detail = _detail(
            _refusal(ws, "fauna.folders.update",
                     {"name": "dav-then-pub", "audience": "public"})
        )
        assert "turn off WebDAV serving first" in detail, detail

        # ── Pair 2, order A: public first, then the paywall. ──
        ws.call(
            "fauna.folders.create",
            {"name": "pub-then-pay",
             "audience": "public"},
        )
        ws.call("fauna.folders.update", {"name": "pub-then-pay", "website_enabled": True})
        detail = _detail(
            _refusal(ws, "fauna.folders.set_web_paywall",
                     {"name": "pub-then-pay", "tier": TIER})
        )
        assert "make it private or shared first" in detail, detail

        # ── Pair 2, order B: the paywall first, then public. ──
        ws.call(
            "fauna.folders.create",
            {"name": "pay-then-pub"},
        )
        ws.call("fauna.folders.update", {"name": "pay-then-pub", "website_enabled": True})
        ws.call("fauna.folders.set_web_paywall", {"name": "pay-then-pub", "tier": TIER})
        detail = _detail(
            _refusal(ws, "fauna.folders.update",
                     {"name": "pay-then-pub", "audience": "public"})
        )
        assert "clear the paywall first" in detail, detail

        # None of the four refusals moved anything: the folders are exactly as
        # they were. A refusal that half-applied would be the worse bug.
        rows = {f["name"]: f for f in ws.call("fauna.folders.list", {})["folders"]}
        assert rows["pub-then-dav"].get("audience") == "public", rows["pub-then-dav"]
        assert rows["pub-then-dav"]["webdav_enabled"] is False, rows["pub-then-dav"]
        assert rows["dav-then-pub"]["webdav_enabled"] is True, rows["dav-then-pub"]
        assert rows["dav-then-pub"].get("audience") != "public", rows["dav-then-pub"]
        assert rows["pub-then-pay"].get("audience") == "public", rows["pub-then-pay"]
        assert rows["pay-then-pub"].get("audience") != "public", rows["pay-then-pub"]
