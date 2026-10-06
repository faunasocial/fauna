"""A folder that keeps no bytes on the nest refuses to also be served out —
all three serving surfaces, **both orders**, on a real ``fauna-nest`` binary.

Owner doc: ``docs/goal/behavior/file-sync.md`` § Content residency — *"v1 scope
cuts (pairwise refusals, both directions, each refutable): metadata-only ⊕
``website_enabled``, ⊕ ``webdav_enabled``, ⊕ paywall — each serving surface
reads bytes off the nest store, and a site or DAV mount that is up only while
the owner's laptop is on is a broken serving promise; refuse whichever side
moves second"*, and ``ui/folders.md`` § Audience and website serving — *"the
refusal text names the repair"*.

**Six refusals, not three.** "Whichever side moves second" is two distinct
code paths per surface, in two different handlers: the ``metadata_only``-moves-
second arms live in ``folder_handlers.rs``'s residency branch of
``fauna.folders.update``, the serving-moves-second arms in that same handler's
website/WebDAV branches and in ``fauna.folders.set_web_paywall`` — which is why
each surface gets both orders here. A one-directional test passes against an
implementation that refuses only the order it happened to try.

**Why this is an API-tier file rather than an extension of
``tests/test_folder_residency_control.py``.** That test drives the *app* UI
(the ``folder-nest-residency-select`` control and its arm-then-confirm), which
is the right venue for the ``[app]`` promise and the wrong one for this
``[nest]`` outcome: it asserts one arm-before-evict case and the flip back, it
cannot reach the paywall surface at all, and an app-surface test cannot witness
a nest refusal the app never offers a button for.

Every refusal is checked for **both** halves of the promise: the typed code
(so a client can branch on it) and the repair named in the free-form detail
(so the user is told what to turn off). The detail is where the repair lives —
the code says only *that* it refused.
"""

import pytest

import fauna_ffi

from clients._ws_rpc_core import RpcCallError
from common.auth import create_actor_and_register

from tests.api.test_web_paywall_folder import _actor_client

pytestmark = pytest.mark.tier_3

TIER = "residency-tier"
REFUSED = "fauna.folders.invalid_request"


def _create(ws, name: str) -> None:
    ws.call(
        "fauna.folders.create",
        {"name": name},
    )


def _update(ws, name: str, **fields) -> dict:
    return ws.call("fauna.folders.update", {"name": name, **fields})


def _folder(ws, name: str) -> dict:
    """This folder's row as the owner's own app reads it (`fauna.folders.list`).

    The update reply is a bare ``{ok}``, so the only witness that a refusal did
    not half-apply is the projection every client renders from.
    """
    reply = ws.call("fauna.folders.list", {})
    return next(f for f in reply["folders"] if f["name"] == name)


def _refusal(ws, kind: str, payload: dict) -> RpcCallError:
    with pytest.raises(RpcCallError) as err:
        ws.call(kind, payload)
    return err.value


def _assert_named_repair(err: RpcCallError, repair: str, what: str) -> None:
    """Both halves of every pairwise refusal: the typed code and the repair."""
    assert err.code == REFUSED, f"{what}: expected {REFUSED}, got {err.code}"
    detail = (err.details or "").lower()
    assert repair in detail, (
        f"{what}: the refusal must name what to change — expected {repair!r} "
        f"in {err.details!r}"
    )


@pytest.mark.feature("folders")
def test_a_no_rest_folder_refuses_to_be_served_whichever_moves_second(nest_instance):
    """Website, WebDAV and paywall, each refused in both orders — six refusals."""
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    owner = create_actor_and_register(port, admin_signing_key=admin_sk)

    with _actor_client(url, owner) as ws:
        # The paywall arms need a real tier of the caller's own — the
        # entitlement seam `set_web_paywall` resolves before it stamps.
        ws.call(
            "fauna.subscriptions.tiers.create",
            {
                "name": TIER,
                "rank": 1,
                "description": None,
                "price_hint": "5/mo",
                "payment_url": "https://example.invalid/pay",
                "auto_approve": False,
                # The required birth KeyBlob (empty roster).
                "encrypted_upload": fauna_ffi.build_tier_birth_upload(bytes(owner["signing_key"]), TIER, b"\x5a" * 32),
            },
        )

        # ── WEBSITE ──────────────────────────────────────────────────────────
        # (a) residency moves second: a website-serving folder cannot go
        # metadata-only.
        _create(ws, "site-then-residency")
        _update(ws, "site-then-residency", website_enabled=True)
        _assert_named_repair(
            _refusal(
                ws,
                "fauna.folders.update",
                {"name": "site-then-residency", "residency": "metadata_only"},
            ),
            "turn off website serving first",
            "website serving on, residency second",
        )
        # …and the folder is left as it was: a refusal must not half-apply.
        row = _folder(ws, "site-then-residency")
        assert row["website_enabled"] is True, row
        assert row.get("residency", "") in ("", "full"), (
            f"a refused flip must not rest a residency: {row.get('residency')!r}"
        )

        # (b) serving moves second: a metadata-only folder cannot serve a site.
        _create(ws, "residency-then-site")
        _update(ws, "residency-then-site", residency="metadata_only")
        _assert_named_repair(
            _refusal(
                ws,
                "fauna.folders.update",
                {"name": "residency-then-site", "website_enabled": True},
            ),
            "set residency back to full first",
            "residency metadata-only, website second",
        )

        # ── WEBDAV ───────────────────────────────────────────────────────────
        # (c) residency moves second.
        _create(ws, "dav-then-residency")
        _update(ws, "dav-then-residency", webdav_enabled=True)
        _assert_named_repair(
            _refusal(
                ws,
                "fauna.folders.update",
                {"name": "dav-then-residency", "residency": "metadata_only"},
            ),
            "turn off webdav serving first",
            "WebDAV serving on, residency second",
        )

        # (d) serving moves second.
        _create(ws, "residency-then-dav")
        _update(ws, "residency-then-dav", residency="metadata_only")
        _assert_named_repair(
            _refusal(
                ws,
                "fauna.folders.update",
                {"name": "residency-then-dav", "webdav_enabled": True},
            ),
            "set residency back to full first",
            "residency metadata-only, WebDAV second",
        )

        # ── PAYWALL ──────────────────────────────────────────────────────────
        # (e) residency moves second. Reaching this arm needs the paywall set
        # while BOTH toggles are off, because the residency branch checks
        # website → WebDAV → paywall in that order and returns on the first
        # hit: with the site still on, (a)'s refusal would fire instead and
        # this arm would never be exercised. A paywall designation survives the
        # website switch going back off (only the ON transition is gated), so
        # that is the door production leaves open to a paywalled, unserved set.
        _create(ws, "paywall-then-residency")
        _update(ws, "paywall-then-residency", website_enabled=True)
        ws.call(
            "fauna.folders.set_web_paywall",
            {"name": "paywall-then-residency", "tier": TIER},
        )
        _update(ws, "paywall-then-residency", website_enabled=False)
        _assert_named_repair(
            _refusal(
                ws,
                "fauna.folders.update",
                {"name": "paywall-then-residency", "residency": "metadata_only"},
            ),
            "clear the paywall first",
            "paywall set, residency second",
        )

        # (f) serving moves second: a metadata-only folder cannot be paywalled.
        # This one is refused in the OTHER handler, which is the whole reason
        # it is asserted separately — `set_web_paywall` checks residency before
        # its own website-toggle gate, so the update handler's refusal cannot
        # stand in for it.
        _create(ws, "residency-then-paywall")
        _update(ws, "residency-then-paywall", website_enabled=True)
        _update(ws, "residency-then-paywall", website_enabled=False)
        _update(ws, "residency-then-paywall", residency="metadata_only")
        _assert_named_repair(
            _refusal(
                ws,
                "fauna.folders.set_web_paywall",
                {"name": "residency-then-paywall", "tier": TIER},
            ),
            "set residency back to full first",
            "residency metadata-only, paywall second",
        )

        # ── …and none of this forbids the legal combinations. Without this the
        # six assertions above are equally satisfied by a nest that refuses
        # every residency flip and every serving toggle. ──
        _create(ws, "full-and-served")
        assert _update(ws, "full-and-served", website_enabled=True)["ok"]
        _create(ws, "no-rest-unserved")
        assert _update(ws, "no-rest-unserved", residency="metadata_only")["ok"]
        # And the repair each refusal names actually works: turn the site off,
        # and the same flip that was refused in (a) goes through.
        assert _update(ws, "site-then-residency", website_enabled=False)["ok"]
        assert _update(ws, "site-then-residency", residency="metadata_only")["ok"]
