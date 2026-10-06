"""tier_3 e2e: the web-content-hosting client authoring UI
(``web-content-hosting.md`` § Admin apex hosting / § Published-post management).

Two UI → nest-RPC round-trips:

- **User subdomain toggle** (``web-settings`` page → ``web-settings-subdomain-toggle``):
  flip the per-user opt-in and assert the toggle reflects the nest's persisted
  state. The toggle's ``state`` attr is written **only** from a nest-confirmed
  snapshot (non-optimistic — the render runs off the reply ``fauna.web.set_subdomain_enabled``
  echoes), so reading it back proves the round-trip.

- **Admin apex picker** (``admin-web`` page → ``admin-web-apex-actor-select``):
  designate an actor as the deployment apex, assert it persists over
  ``fauna.web.get_apex_actor`` (the authoritative wire surface), then clear it
  ("None") and assert cleared. Mirrors ``test_admin_catch_all.py``.

``nest_instance`` is session-scoped; the apex is a nest-wide singleton, so the
test designates then clears (leaving it as it found it).
"""

import time

import pytest

from conftest import WEB_HOSTING_DOMAIN
from helpers.admin_wire import admin_user, apex_actor, picker_option
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from i18n.strings import S

# All five wired clients render both surfaces: the admin `admin-web` apex picker
# (admin-shell detail pane) and the user `web-settings` subdomain toggle. macОS was
# historically deferred (its single-scroll PreferencesView put the subdomain toggle
# ~5000px down a *nested* ScrollView, unhittable — the macOS Settings shell track);
# that is now RESOLVED — both tests confirmed green `--client macos` AND
# `--client ios` (2026-06-17, in-process driver). iOS
# renders web-settings as its own NavigationStack screen (not the macOS nested
# PreferencesView), so the toggle is directly reachable. Both functions share the
# same client set, so the markers live module-level (android is the remaining lift).
# `tui` added 2026-07-29 with the two pages themselves (`admin/web.rs` +
# `settings/web.rs`) — the last app owing `admin-web` per `admin.md` § 7 Web.
# ⚠ The app markers are PER TEST, not module-level, and that is load-bearing:
# `pytest_collection_modifyitems` reads `item.iter_markers()`, which is the UNION
# of module and function marks — so a module-level app list cannot be narrowed by
# a function-level one. A module list here therefore silently collected the
# tui-only Published-posts tests on all six apps (`--app linux` collected
# `test_published_post_management_section[linux]`, which linux does not build),
# while the section's own comment claimed the opposite. Add the marks to the
# tests that actually run on each app.
pytestmark = [pytest.mark.tier_3]

# The six apps that render BOTH original surfaces: the admin `admin-web` apex
# picker and the user `web-settings` subdomain toggle.
_ALL_WIRED_APPS = [
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
]


def all_wired_apps(fn):
    """Mark a test for all six apps that render the original two surfaces."""
    for mark in _ALL_WIRED_APPS:
        fn = mark(fn)
    return fn


def _first_actor(nest_instance):
    """``(option, actor_id_bytes)`` of the deployment's admin actor, as the apex
    picker offers it — found across every ``fauna.admin.users.list`` page
    (``helpers.admin_wire.admin_user``)."""
    admin = admin_user(nest_instance)
    return picker_option(admin), admin["actor_id"]


@all_wired_apps
@pytest.mark.feature("personal-website")
def test_user_subdomain_toggle_round_trip(logged_in_app, nest_instance):
    """Flip the per-user subdomain opt-in on the web-settings page → assert the
    toggle reflects the nest's persisted (default-OFF) state."""
    app = logged_in_app
    app.driver.set_state({
        "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "web"}]},
    })
    app.driver.wait_for("web-settings-subdomain-toggle", timeout=15.0)

    # Default OFF.
    assert app.driver.get_attr("web-settings-subdomain-toggle", "state") == "off", (
        "subdomain toggle should default OFF"
    )

    # Turn it ON; the `state` attr only flips once the nest confirms the write.
    app.driver.click("web-settings-subdomain-toggle")
    wait_until(
        lambda: app.driver.get_attr("web-settings-subdomain-toggle", "state") == "on",
        RPC_ROUNDTRIP_S,
        diagnose=lambda: "subdomain toggle did not reflect ON after the nest round-trip",
    )

    # Turn it back OFF (leave the actor as found).
    app.driver.click("web-settings-subdomain-toggle")
    wait_until(
        lambda: app.driver.get_attr("web-settings-subdomain-toggle", "state") == "off",
        RPC_ROUNDTRIP_S,
        diagnose=lambda: "subdomain toggle did not reflect OFF after the nest round-trip",
    )


@all_wired_apps
@pytest.mark.feature("admin-front-page")
def test_admin_apex_designate_and_clear(admin_app, nest_instance):
    """Designate the apex actor via the picker → assert it persists over
    ``fauna.web.get_apex_actor`` → clear it ("None") → assert cleared."""
    label, actor_id = _first_actor(nest_instance)
    assert label, "admin actor has an empty label — cannot select it in the picker"

    admin_app.admin.navigate_web()
    admin_app.driver.wait_for("admin-web-apex-actor-select", timeout=15.0)

    # --- Designate the actor as the apex ---
    admin_app.admin.set_apex_actor(label)
    persisted = wait_until(
        lambda: apex_actor(nest_instance),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"get_apex_actor never became non-empty after designating {label!r}",
    )
    assert persisted == actor_id, (
        f"after designating {label!r} via admin-web-apex-actor-select, "
        f"get_apex_actor = {persisted!r}, expected {actor_id!r}"
    )

    # --- Clear it (the "None" option reverts to the built-in info page) ---
    admin_app.admin.set_apex_actor(S.admin.web_page.apex_none)
    wait_until(
        lambda: apex_actor(nest_instance) is None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"after selecting None, apex actor not cleared; still {apex_actor(nest_instance)!r}",
    )


# ── The Published-posts management section (tui led) ────────────────────
#
# `web-content-hosting.md` § Published-post management. tui was the lead app
# (2026-08-02); linux followed (2026-08-13); macos+ios landed
# 2026-08-14; web/android/windows remain. Add the marker to the
# tests that actually render the section as each leg lands.
#
# **Why its own nest.** The shared session `nest_instance` runs with an EMPTY
# handle domain, so the shared `site_link_view` composes
# `https://<handle>.127.0.0.1:PORT/` — a URL the serving layer's subdomain
# resolver can never match. Proving the copied link is a link that SERVES needs
# a handle-domain nest (`web_hosting_nest`), the same reason
# `tests/api/test_web_subdomain_hosting.py` starts its own.
#
# **Why these tests seed the post over the wire.** They test the
# `web-settings` section on its own, not the publish gesture — the feed
# ⋯-overflow's own publish/unpublish/copy verbs get their end-to-end coverage
# in `test_publishing_a_post_from_the_feed_overflow_menu` and
# `test_the_overflow_copy_verb_is_dead_without_a_serving_origin` below.
# Authoring + publishing are PRECONDITION setup here — the E2E rules'
# fixture-setup carve-out — while every mutation *under test* (the origin
# opt-in, both copies, the takedown) is driven through the app UI exactly as a
# user would. `test_a_copied_paywall_link_serves_the_full_body` additionally
# seeds the web-serve holder's capability grant over the wire — there is no
# client UI for it yet (`fauna_capability_build_post_grant` is the Python e2e
# twin of a production surface that does not exist), the same carve-out
# `test_gated_post_compose.py` uses for the subscriber-side KeyBlob grant.


def _user_client(nest, user):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(user["signing_key"].verify_key),
        signing_key=bytes(user["signing_key"]),
    )


def _publish(nest, user, post_id_hex, slug):
    with _user_client(nest, user) as ws:
        reply = ws.call(
            "fauna.web.publish.set",
            {"post_id": bytes.fromhex(post_id_hex), "slug": slug},
        )
    # The nest's EFFECTIVE slug, which is what the UI must build its link from.
    return reply["slug"]


def _get_with_host(nest_url, url, extra_query=""):
    """GET the copied `url` from the loopback nest, routing by its own Host.

    The copied string is `https://<handle>.<domain>/post/<slug>.html`; the nest
    is bound to loopback with no TLS, so the request goes to the nest's address
    with the copied URL's host and path carried explicitly. That is the point of
    the assertion: the host and path the UI handed the user are the ones the nest
    answers on.
    """
    import urllib.error
    import urllib.parse
    import urllib.request

    parsed = urllib.parse.urlsplit(url)
    target = nest_url.rstrip("/") + parsed.path + (parsed.query and "?" + parsed.query or "")
    if extra_query:
        target += ("&" if "?" in target else "?") + extra_query
    req = urllib.request.Request(target, method="GET", headers={"Host": parsed.netloc})
    try:
        resp = urllib.request.urlopen(req)
        return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")


def _upload_sealed_blob(port: int, token: str, data: bytes) -> str:
    """`POST /api/v1/blob` (multipart `sidecar` + `bytes`) for an AEAD-sealed
    period-restricted-post blob — the shape `fauna_ffi.build_gated_post`
    produces and the strict blob verifier expects (sealed class ⇒ sidecar MIME
    is octet-stream; the real MIME rides inside the seal). Returns the hex
    hash. Mirrors `tests/api/test_web_paywall.py::_upload_sealed_blob`."""
    import json
    import cbor2
    import urllib.request

    from common.auth import port_base_url

    sidecar = cbor2.dumps(
        {
            "class": "PeriodRestrictedPost",
            "mime": "application/octet-stream",
            "has_c2pa": False,
            "thumbnail_hash": None,
        },
        canonical=True,
    )
    boundary = "faunapaywalltestboundary"
    body = b"".join(
        [
            f"--{boundary}\r\n".encode(),
            b'Content-Disposition: form-data; name="sidecar"\r\n\r\n',
            sidecar,
            f"\r\n--{boundary}\r\n".encode(),
            b'Content-Disposition: form-data; name="bytes"\r\n\r\n',
            data,
            f"\r\n--{boundary}--\r\n".encode(),
        ]
    )
    req = urllib.request.Request(
        f"{port_base_url(port)}/api/v1/blob",
        data=body,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": f"multipart/form-data; boundary={boundary}",
        },
        method="POST",
    )
    resp = urllib.request.urlopen(req)
    return json.loads(resp.read())["hash"]


@pytest.mark.tier_3
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("personal-website")
def test_published_post_management_section(web_hosting_app, web_hosting_nest):
    """The lead-app journey: a creator sees their published post, copies a link
    that actually serves, and takes the page down — all from `web-settings`.

    Also pins the ratified "legal but unreachable" rule: with the subdomain
    opt-in OFF the row's copy affordance is DEAD, because publishing without a
    serving origin produces no shareable link and the UI must say so rather than
    hand out a URL that cannot load.
    """
    app = web_hosting_app
    nest = web_hosting_nest
    user = nest["user"]
    web = app.web_settings

    from tests.api import ws_api
    from tests.api.bare import sign_and_encode_post

    now_us = int(time.time() * 1_000_000)
    post_id = ws_api.create_post(
        _port(nest),
        user,
        sign_and_encode_post(user["signing_key"], now_us, "a post worth publishing"),
    )

    web.navigate()
    web.wait_for_published_posts_section()
    # A creator who has published nothing sees the empty state, not a blank.
    assert web.is_empty_state_visible(), (
        "a hydrated page with no published posts must render its empty state; "
        f"rows={web.published_slugs()} error={app.error_text()!r}"
    )

    slug = _publish(nest, user, post_id, "my-first-page")
    assert slug == "my-first-page"

    # Re-enter the page so it re-reads `publish.list` — the same self-hydrate
    # every visit runs, which is how a sibling device's publish shows up.
    web.navigate()
    index = web.wait_for_slug(slug)
    assert not web.is_empty_state_visible()
    assert web.gated_tier(index) == "", "an ungated row must advertise no tier"
    assert not web.offers_paywall_link(index), (
        "Copy paywall link is published-AND-gated only — an ungated post has no "
        "sealed body to hand out"
    )

    # ── The origin: dead affordance → live, driven entirely through the UI ──
    web.set_subdomain_enabled(False)
    index = web.row_index_for(slug)
    assert not web.copy_link_enabled(index), (
        "with no serving origin the copy affordance must be dead — publishing is "
        "legal but unreachable, and a dead link on the clipboard is worse than none"
    )

    web.set_subdomain_enabled(True)
    index = web.row_index_for(slug)
    assert web.copy_link_enabled(index), (
        "opting into subdomain hosting must give the row a live link; "
        f"error={app.error_text()!r}"
    )

    # ── The copied string ──
    copied = web.copy_web_link(index)
    handle = user["handle"]
    assert copied.endswith(f"/post/{slug}.html"), (
        f"the copied link must address the nest's EFFECTIVE slug, not the "
        f"requested one: {copied!r}"
    )
    assert copied.startswith(f"https://{handle}."), (
        f"the copied link must sit on this actor's own subdomain origin: {copied!r}"
    )

    # ── The takedown: one tap, and the row leaves ──
    web.unpublish(index)
    web.wait_for_slug_absent(slug)
    assert web.is_empty_state_visible(), (
        "taking the last post down must return the section to its empty state"
    )
    # And the nest agrees — the row left because the takedown committed, not
    # because the UI removed it locally.
    with _user_client(nest, user) as ws:
        rows = ws.call("fauna.web.publish.list", {}).get("posts", [])
    assert not rows, f"the takedown moved the UI but not the nest: {rows}"


# The apps whose `web-settings` paints `web-settings-render-status`
# (`web-content-hosting.md` § Implementation status today). One app per landing
# in the trickle-down; the set disappears when it holds all seven.
_RENDER_STATUS_BUILT = frozenset({"tui", "linux", "web", "android", "macos", "ios"})


@pytest.mark.tier_3
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("personal-website")
def test_a_site_the_nest_took_dark_tells_its_author(web_hosting_app, web_hosting_nest):
    """A site the nest cleared after a failed render says so on `web-settings`,
    and stops saying so once a render has brought it back.

    From the app a dark site used to look exactly like a healthy one
    (``web-content-hosting.md`` § Routing, render, serving → *A blanked site
    tells its author*). The dark state is installed by the nest's own
    fail-closed writer (``web_blank_site_test_hook.rs`` — a running nest's
    storage cannot honestly be made to fail from outside); that a failed render
    reaches that writer, and what ``publish.list`` then answers, is pinned
    in-process by ``conformance_web.rs``. What only a journey can show is the
    app reading it — and the restore arriving through a real door: the
    takedown below is driven through the UI, its render restores the site, and
    the list re-read that follows clears the line with no re-visit.
    """
    app = web_hosting_app
    nest = web_hosting_nest
    user = nest["user"]
    web = app.web_settings

    from conftest import _bridge_admin_post
    from helpers.app_surface import app_name, skip_unbuilt
    from tests.api import ws_api
    from tests.api.bare import sign_and_encode_post

    if app_name(app.driver) not in _RENDER_STATUS_BUILT:
        skip_unbuilt(
            app.driver,
            surface="web-settings-render-status",
            detail="the status line is built on tui, linux, web, android, macos and ios; windows follows in trickle-down",
            tracked="web-content-hosting.md § Implementation status today",
        )

    now_us = int(time.time() * 1_000_000)
    post_id = ws_api.create_post(
        _port(nest),
        user,
        sign_and_encode_post(user["signing_key"], now_us, "a post on a site about to go dark"),
    )
    slug = _publish(nest, user, post_id, "dark-site-post")

    web.navigate()
    web.wait_for_published_posts_section()
    index = web.wait_for_slug(slug)
    assert web.render_status_text() == "", (
        f"a healthy site must paint no render status: {web.render_status_text()!r}"
    )

    blanked = _bridge_admin_post(
        nest["url"],
        nest["admin"]["token"],
        "/api/v1/test/web/blank-site",
        {"actor_id": bytes(user["signing_key"].verify_key).hex()},
    )
    assert blanked.get("ok"), f"the nest refused to blank the site: {blanked}"

    # Re-enter so the page re-reads `publish.list` — the read the flag rides.
    web.navigate()
    web.wait_for_published_posts_section()
    index = web.wait_for_slug(slug)
    assert web.render_status_text() == S.web_settings.render_status_down, (
        "a site the nest took dark must say so; "
        f"status={web.render_status_text()!r} error={app.error_text()!r}"
    )
    with _user_client(nest, user) as ws:
        assert ws.call("fauna.web.publish.list", {}).get("rendered_pages_down") is True

    # The takedown's render is one of the renders that restores a site, and
    # the page re-reads the list behind it.
    web.unpublish(index)
    web.wait_for_slug_absent(slug)
    web.wait_for_render_status_absent()
    with _user_client(nest, user) as ws:
        reply = ws.call("fauna.web.publish.list", {})
    assert not reply.get("rendered_pages_down"), (
        f"the UI cleared the status but the nest still owes the restore: {reply}"
    )


@pytest.mark.tier_3
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("personal-website")
def test_a_copied_web_link_actually_serves(web_hosting_app, web_hosting_nest):
    """The copied public link must be a link that LOADS — the external black-box
    half of the surface's e2e contract (`web-content-hosting.md`
    § Published-post management: the copied public URL serves the teaser, via the
    Host-header GET pattern of `tests/api/test_web_subdomain_hosting.py`).

    **This was `xfail(strict=True)` until 2026-08-09**, and the finding it carried
    was right that the link was dead but wrong about why, on every app but tui:

    - `web_hosting_nest` is dialed on **loopback** while serving `web.test`, so
      tui's `url_host(node_url)` composed `https://<handle>.127.0.0.1:PORT/`;
    - the other six apps never used the dialed host at all — they passed their
      **cached sign-in `domain`**, which is `AppState::handle_domain()`. That is
      right here, and wrong on a *domainless* nest, where it is the literal
      `"localhost"` placeholder and `<handle>.localhost` is the one suffix the
      resolver never strips;
    - and the prescribed fix — `nest.info`'s `registration.handle_domain` — is the
      *boot seed*, one tier below the claimed `identity_domain` the resolver
      actually keys off, so it would still be wrong on a box claimed after boot.

    All three are now replaced by the nest reporting
    `NestInfoReply.web_serving_domain`, read from the same `web_serving_domain()`
    accessor its `HostResolver` uses.
    """
    app = web_hosting_app
    nest = web_hosting_nest
    user = nest["user"]
    web = app.web_settings

    from tests.api import ws_api
    from tests.api.bare import sign_and_encode_post

    now_us = int(time.time() * 1_000_000)
    post_id = ws_api.create_post(
        _port(nest),
        user,
        sign_and_encode_post(user["signing_key"], now_us, "a post worth serving"),
    )
    slug = _publish(nest, user, post_id, "servable-page")

    web.navigate()
    web.set_subdomain_enabled(True)
    index = web.wait_for_slug(slug)
    copied = web.copy_web_link(index)

    status, body = _get_with_host(nest["url"], copied)
    assert status == 200, (
        f"the link the UI put on the user's clipboard did not serve: {status} "
        f"for {copied!r} -- {body[:300]}"
    )
    assert "a post worth serving" in body, (
        f"the copied link served, but not this post's page: {body[:300]}"
    )

    # Leave the actor as found: `web_hosting_nest`/`web_hosting_app` are
    # module-scoped and shared by every test in this module — including
    # across a combined multi-app run (`--app macos,ios` parametrizes the
    # SAME nest for both), so a leaked "servable-page" publish collides with
    # the next app's own attempt to publish the identical slug
    # (`fauna.web.internal: publish_web_post upsert_link`).
    web.unpublish(index)
    web.wait_for_slug_absent(slug)


@pytest.mark.tier_3
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("personal-website")
def test_a_copied_paywall_link_serves_the_full_body(web_hosting_app, web_hosting_nest):
    """The *Copy paywall link* value must be a link that unseals the FULL
    body — the gated twin of `test_a_copied_web_link_actually_serves` above
    (`web-content-hosting.md` § Published-post management; the mint/serve/
    revoke mechanism itself is proven nest-side end to end by
    `tests/api/test_web_paywall.py::test_web_paywall_teaser_token_and_revoke`,
    which this test does not re-prove — only the APP UI's own round trip:
    clicking *Copy paywall link* must hand the user a link that actually
    opens the paid content, not just a live-looking button).

    Only the public/teaser link was proven to serve before this (the
    step 5 residual); this was the one path still unassertable.
    """
    app = web_hosting_app
    nest = web_hosting_nest
    user = nest["user"]
    web = app.web_settings

    import uuid

    from tests.api import ws_api
    import fauna_ffi

    tier = f"paywall-e2e-{uuid.uuid4().hex[:8]}"
    tier_rank = 2
    preview = "A gated post worth paying for"
    full_marker = "the-tokened-link-full-body-marker"
    full_body = f"{preview}\n\nHere is {full_marker}, for token holders only."
    period_key = bytes(range(32))
    key_blob_ref = b"\x22" * 32
    secret = bytes(user["signing_key"])

    with _user_client(nest, user) as ws:
        ws.call(
            "fauna.subscriptions.tiers.create",
            {
                "name": tier,
                "rank": tier_rank,
                "description": None,
                "price_hint": "5 EUR / month",
                "payment_url": "https://pay.example/gold",
                "auto_approve": False,
                # The required birth KeyBlob (empty roster).
                "encrypted_upload": fauna_ffi.build_tier_birth_upload(secret, tier, period_key),
            },
        )

    post_bytes, encrypted_blob = fauna_ffi.build_gated_post(
        secret, preview, full_body, tier, tier_rank, key_blob_ref, period_key
    )
    blob_hash = _upload_sealed_blob(_port(nest), user["token"], encrypted_blob)
    assert len(blob_hash) == 64

    post_id = ws_api.create_post(_port(nest), user, post_bytes)
    slug = _publish(nest, user, post_id, f"gated-{uuid.uuid4().hex[:8]}")

    # ── Grant the web-serve holder the tier's period key — fixture setup, per
    # the module docstring above: no client UI mints this grant yet. ──
    now = int(time.time())
    with _user_client(nest, user) as ws:
        holder = ws.call(
            "fauna.bridges.fetch_bridge_pubkey",
            {"bridge_role": "content-processor", "bridge_id": "web-serve"},
        )
        holder_x25519 = bytes(holder["x25519_pubkey"])
        holder_ek = bytes(holder["mlkem_ek"]) if holder.get("mlkem_ek") else None
        grant_blob = fauna_ffi.build_post_grant(
            bytes(user["actor_id_bytes"]),
            uuid.uuid4().bytes,
            holder_x25519,
            holder_ek,
            now,
            now + 3600,
            tier,
            period_key,
        )
        mint = ws.call("fauna.capabilities.mint", {"grant_blob": grant_blob})
        assert mint.get("ok") is True, mint

    # ── The mutation under test: the UI mints the tokened link over the wire. ──
    web.navigate()
    web.set_subdomain_enabled(True)
    index = web.wait_for_slug(slug)
    assert web.gated_tier(index) == tier, (
        f"the published row must advertise its gating tier; error={app.error_text()!r}"
    )
    assert web.offers_paywall_link(index), (
        f"a published+gated row must offer Copy paywall link; error={app.error_text()!r}"
    )
    tokened = web.copy_paywall_link(index)
    assert "token=" in tokened, f"the copied paywall link must carry a token: {tokened!r}"

    status, body = _get_with_host(nest["url"], tokened)
    assert status == 200, (
        f"the tokened link the UI put on the user's clipboard did not serve: "
        f"{status} for {tokened!r} -- {body[:300]}"
    )
    assert full_marker in body, (
        f"the tokened link served, but not the full body: {body[:300]}"
    )

    # ── Negative control: the plain (token-less) link must still be the
    # teaser — proves the assertion above is about the TOKEN, not a page that
    # would serve the full body regardless. ──
    plain = web.copy_web_link(index)
    status, teaser_body = _get_with_host(nest["url"], plain)
    assert status == 200, f"the plain link did not serve: {status} -- {teaser_body[:300]}"
    assert full_marker not in teaser_body, (
        f"the token-less link must stay the teaser, not the full body: "
        f"{teaser_body[:300]}"
    )
    assert preview in teaser_body, (
        f"the token-less link's teaser must show the preview: {teaser_body[:300]}"
    )


@pytest.mark.tier_3
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("personal-website")
def test_publishing_a_post_from_the_feed_overflow_menu(
    web_hosting_app, web_hosting_nest
):
    """The ⋯-overflow half of the surface, end to end and entirely through the
    UI (`web-content-hosting.md` § Published-post management — the per-post
    verbs; presence rules `ui/feed.md` § User actions).

    This is the journey no app could run before 2026-08-13: the four verbs were
    specified 2026-08-02 and built nowhere, which left `fauna.web.publish.set`
    the one kind of this family **no app UI drove at all** — the sibling
    `web-settings` section only ever listed, unpublished and minted. So the
    publish leg of every other test on this page is an API shortcut; here it is
    a click.

    The cross-surface assertion is the point of doing it here rather than in a
    tui unit test: a post published from the FEED must appear in the
    `web-settings` section, because both surfaces are views of one
    `fauna.web.publish.list`. And the copied link is checked by loading it — the
    external black-box half of the e2e contract.
    """
    app = web_hosting_app
    nest = web_hosting_nest
    web = app.web_settings
    feed = app.feed

    # The origin first: with no serving origin the copy verbs are legally dead,
    # and this test is about the live path (the dead path is its own test).
    web.navigate()
    web.wait_for_published_posts_section()
    web.set_subdomain_enabled(True)
    # ⚠ Baseline, NOT an empty-state precondition. This actor is shared across
    # the module and `test_a_copied_web_link_actually_serves` leaves its own
    # `servable-page` published, so "nothing published yet" holds when this test
    # runs alone and is false in a full-file run. Every assertion below is
    # relative to what was already there — the order-independent shape.
    baseline = set(web.published_slugs())

    body = "a post published from the overflow menu"
    app.driver.navigate_to("feed")
    feed.create_post(text=body)
    assert feed.wait_for_post_text(body), (
        f"the post never landed in the feed; error={app.error_text()!r}"
    )

    # ── Unpublished: exactly one verb, and no link affordances ──
    feed.open_post_actions(index=0)
    assert feed.web_verb_visible("publish-web"), (
        "an own unpublished post must offer Publish to web; "
        f"error={app.error_text()!r}"
    )
    for absent in ("unpublish-web", "copy-web-link", "copy-paywall-link"):
        assert not feed.web_verb_visible(absent), (
            f"{absent} describes a published page that does not exist yet"
        )

    # ── Publish, and the sibling surface learns about it ──
    feed.publish_post_to_web(index=0)
    web.navigate()
    added = wait_until(
        lambda: (set(web.published_slugs()) - baseline) or None,
        web.PAGE_READY_BUDGET_S,
        diagnose=lambda: (
            "a post published from the feed ⋯ menu never reached the "
            f"web-settings section; rows={web.published_slugs()} "
            f"baseline={sorted(baseline)} error={app.error_text()!r}"
        ),
    )
    assert len(added) == 1, f"one publish must add exactly one row, got {added}"
    slug = added.pop()

    # ── The verbs flip on the card, off the same publish state ──
    app.driver.navigate_to("feed")
    feed.open_post_actions(index=0)
    assert not feed.web_verb_visible("publish-web"), (
        "an already-published post must not offer a second publish"
    )
    for present in ("unpublish-web", "copy-web-link"):
        assert feed.web_verb_visible(present), f"missing {present} after publish"
    assert not feed.web_verb_visible("copy-paywall-link"), (
        "Copy paywall link is published-AND-gated only — this post is ungated"
    )

    # ── The copied link, and it actually serves ──
    copied = feed.copy_web_link(index=0)
    handle = nest["user"]["handle"]
    assert copied == f"https://{handle}.{WEB_HOSTING_DOMAIN}/post/{slug}.html", (
        f"the menu must copy this actor's own serving origin + the nest's "
        f"EFFECTIVE slug: {copied!r}"
    )
    status, served = _get_with_host(nest["url"], copied)
    assert status == 200, (
        f"the link the ⋯ menu put on the clipboard did not serve: {status} for "
        f"{copied!r} -- {served[:300]}"
    )
    assert body in served, (
        f"the copied link served, but not this post's page: {served[:300]}"
    )

    # ── The takedown: one tap, no confirm, and the nest agrees ──
    feed.unpublish_post_from_web(index=0)
    web.navigate()
    wait_until(
        lambda: (slug not in web.published_slugs()) or None,
        web.PAGE_READY_BUDGET_S,
        diagnose=lambda: (
            f"the ⋯-menu takedown never removed {slug!r} from the management "
            f"section; rows={web.published_slugs()} error={app.error_text()!r}"
        ),
    )
    # And the nest agrees — the row left because the takedown COMMITTED, not
    # because the UI dropped it locally. Asserted against this post's slug
    # rather than an empty list, for the shared-actor reason above.
    with _user_client(nest, nest["user"]) as ws:
        rows = ws.call("fauna.web.publish.list", {}).get("posts", [])
    assert slug not in [r.get("slug") for r in rows], (
        f"the takedown moved the UI but not the nest: {rows}"
    )


@pytest.mark.tier_3
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("personal-website")
def test_the_overflow_copy_verb_is_dead_without_a_serving_origin(
    web_hosting_app, web_hosting_nest
):
    """**Publishing with no serving origin is legal but unreachable, and the UI
    must say so** rather than hand out a link that cannot load
    (`web-content-hosting.md` § Published-post management).

    The ⋯-menu twin of the same rule the `web-settings` section is held to. The
    takedown stays live throughout: it needs no origin, and it is the one thing
    a user with an unreachable site may well want.
    """
    app = web_hosting_app
    feed = app.feed
    web = app.web_settings

    web.navigate()
    web.set_subdomain_enabled(True)

    body = "a post with nowhere to serve"
    app.driver.navigate_to("feed")
    feed.create_post(text=body)
    assert feed.wait_for_post_text(body)
    feed.publish_post_to_web(index=0)

    # Take the origin away through the UI — the same control the section's own
    # dead-affordance assertion drives.
    web.navigate()
    web.set_subdomain_enabled(False)

    app.driver.navigate_to("feed")
    feed.open_post_actions(index=0)
    assert feed.web_verb_visible("copy-web-link"), (
        "the verb must still be PRESENT — it is disabled with a reason, not "
        "hidden, so the user learns why there is no link"
    )
    assert not feed.web_verb_enabled("copy-web-link"), (
        "with no serving origin the copy affordance must be dead: a link that "
        f"cannot load is worse on the clipboard than none; error={app.error_text()!r}"
    )
    assert feed.web_verb_enabled("unpublish-web"), (
        "a takedown needs no serving origin"
    )

    # Leave the actor as found: this module's actor is shared, so a leaked
    # takedown-pending post or an OFF subdomain toggle becomes the next test's
    # mystery failure.
    feed.unpublish_post_from_web(index=0)
    web.navigate()
    web.set_subdomain_enabled(True)


@pytest.mark.tier_3
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("personal-website")
def test_the_subdomain_url_row_names_the_host_the_nest_serves_on(
    web_hosting_app, web_hosting_nest
):
    """`web-settings-subdomain-url` must name the nest's SERVING domain.

    The oldest surface on this page, and the one every app renders — which makes
    it the cheapest cross-app proof that the input reaching `subdomain_view` is
    the nest's own answer rather than something the app derived.

    `web_hosting_nest` is the discriminating shape: it serves `web.test` while
    being **dialed on loopback**, so the two candidate values are visibly
    different and an app that reaches for the address it dialed spells `127.0.0.1`
    right here. It is also why this assertion cannot be made on the shared
    `nest_instance`, whose handle domain is empty.

    Marked for linux, tui, web, windows (windows reads `FfiWebClient.ServingDomain`
    instead of the cached sign-in domain — the exact near-miss this test exists to
    catch) and now macos/ios (`WebPublishStore.hydrate`'s serving-domain
    read).
    """
    web = web_hosting_app.web_settings
    web.navigate()
    web.set_subdomain_enabled(True)

    handle = web_hosting_nest["user"]["handle"]
    expected = f"https://{handle}.{WEB_HOSTING_DOMAIN}/"
    url_text = wait_until(
        lambda: web.subdomain_url_text() or None,
        web.PAGE_READY_BUDGET_S,
        diagnose=lambda: (
            "the subdomain URL row never painted; "
            f"error={web_hosting_app.error_text()!r}"
        ),
    )
    assert url_text == expected, (
        f"the URL row must name the host the nest SERVES on ({expected!r}), not "
        f"the address the app dialed ({web_hosting_nest['url']!r}): got {url_text!r}"
    )


def _port(nest):
    """The nest's TCP port, which `ws_api` takes instead of a url."""
    from urllib.parse import urlsplit

    return urlsplit(nest["url"]).port
