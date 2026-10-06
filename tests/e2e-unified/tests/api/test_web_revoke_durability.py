"""A revoke outlives a restart, and a blanked site restores itself at the next
boot — the two *durability* promises of the web-serving plane, on a real
``fauna-nest`` binary that really stops and really comes back.

``web-content-hosting.md`` § Routing, render, serving owns both:

* *A revoke is durable* — a page the author unpublished or deleted is off the
  rendered site, and the **owed-render marker** written in the state change's
  own transaction is drained at boot (``drain_owed_renders``, awaited before the
  nest serves), so a restart can never resurrect it.
* *A blanked site is owed its restore* — a fail-closed clear records one
  ``web_restore_owed`` row in the clear's own transaction, and the **boot
  drain** makes one render attempt per blanked site, with nothing for the
  author to do.

**What these two tests do and do not witness.** Both drive the *durable* state
across a real process restart, which is the half an outside observer can reach
honestly:

* The **torn window** — a nest that stops *between* a revoking door's commit and
  its render — cannot be produced from outside the process (there is no door
  that lands the commit and then withholds the render), and stays pinned in
  ``bins/fauna-nest/tests/conformance_web.rs``.
* The **paced restore retry** (``start_restore_retry``, the between-boots half of
  *A blanked site is owed its restore*) likewise cannot be triggered honestly
  from outside: the only outside door to a dark site,
  ``/api/v1/test/web/blank-site``, deliberately does **not** wake it, and nothing
  else out here can make a render fail. So ``test_a_blanked_site_comes_back_at_the
  _next_boot`` witnesses the **boot-drain half only**; the retry stays
  Rust-pinned. The outcome it witnesses is not narrowed by that — a blanked site
  does come back by itself, with nothing for the author to do — only the *which
  payer* half is.

Process safety: the restart goes through ``common.nest.restart_nest``, which
SIGTERMs the fixture's OWN ``proc`` handle and nothing else — never
``pkill``/``killall``/name-match, which would take sibling sessions' nests down
with it (E2E rule: *Process safety*). The nest is a module-scoped **dedicated**
nest for the same reason: the session-shared ``nest_instance`` is not ours to
stop.
"""

import time

import pytest

from common.auth import create_actor_and_register
from clients.ws_rpc_admin_client import WsRpcAdminClient

from tests.api import ws_api
from tests.api.bare import sign_and_encode_post, sign_and_encode_tombstone
from tests.api.test_web_paywall_folder import _actor_client, _get

pytestmark = pytest.mark.tier_3

BUILTIN_404_MARKER = "404 Not Found"


@pytest.fixture(scope="module")
def durable_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest these tests may stop and start.

    Domainless, so the apex catch-all answers every host and a designated apex
    actor's site serves at ``/`` — the shape ``test_web_folder_audience`` and
    ``test_web_paywall_folder`` already use against the shared nest. What is
    dedicated here is the *process*: ``restart_nest`` stops it, and stopping the
    session-shared nest would stop every sibling test's nest too.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "nest-web-durable"
    )
    try:
        yield nest
    finally:
        cleanup()


def _admin_client(nest) -> WsRpcAdminClient:
    admin_sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(
        nest["url"], actor_id=bytes(admin_sk.verify_key), signing_key=bytes(admin_sk)
    )


def _serve_this_actors_site(nest, actor) -> None:
    """Designate `actor` as the apex actor, so their site answers at `/`.

    Persisted nest-side (`db.set_apex_actor`) as well as pushed to the live
    resolver, which is what makes it survive the restarts below — the boot seed
    reads the same row.
    """
    with _admin_client(nest) as admin:
        admin.call(
            "fauna.web.set_apex_actor", {"actor_id": bytes(actor["actor_id_bytes"])}
        )


def _clear_apex(nest) -> None:
    with _admin_client(nest) as admin:
        admin.call("fauna.web.set_apex_actor", {"actor_id": None})


def _publish(nest, actor, title: str, slug: str, *, offset_us: int = 0) -> str:
    """Create a post and publish it to the web; returns its hex post id.

    Apostrophe-free titles: the default index/post templates HTML-escape them
    (`Alice&#x27;s`), which would defeat a literal substring assertion.
    """
    post_bytes = sign_and_encode_post(
        actor["signing_key"], int(time.time() * 1_000_000) + offset_us, title
    )
    post_id = ws_api.create_post(nest["port"], actor, post_bytes)
    ws_api.web_publish_set(nest["port"], actor, post_id, slug)
    return post_id


def _page(nest, slug: str) -> tuple[int, str]:
    """The rendered per-post page for `slug` — the URL a visitor was handed."""
    status, body, _headers = _get(nest["url"], f"/post/{slug}.html")
    return status, body


def _assert_serves(nest, slug: str, marker: str, when: str) -> None:
    status, body = _page(nest, slug)
    assert status == 200 and marker in body, f"{when}: expected {marker!r}, got {status}: {body}"


def _assert_gone(nest, slug: str, marker: str, when: str) -> None:
    status, body = _page(nest, slug)
    assert status != 200 or marker not in body, (
        f"{when}: a withdrawn page is still loading for visitors: {status} {body}"
    )


def _restart(nest) -> None:
    """Restart the nest and drop every cached WS client onto the new process."""
    from common.nest import restart_nest

    ws_api.close_all_ws()
    restart_nest(nest, graceful=True)


@pytest.mark.feature("personal-website")
def test_an_unpublished_or_deleted_page_stays_gone_across_a_restart(durable_nest):
    """A page the author unpublishes, and a published post the author deletes,
    both stop loading for visitors — and are still gone after the nest restarts.

    Both verbs the outcome names are exercised against one site, because both are
    *revoking doors* under the same rule and a fix to one that missed the other
    is exactly the half-fix worth catching: `publish.unset` removes the
    `web_published` link, and `fauna.posts.delete`'s cascade removes the same
    link, so the delete's own transaction marks the render owed the same way.

    A third post stays published throughout. Without it the restart assertions
    would pass just as well against a site the boot drain had blanked outright,
    which is the opposite of the promise: the withdrawn pages go, the site stays.
    """
    nest = durable_nest
    actor = create_actor_and_register(
        nest["port"], admin_signing_key=nest["admin"]["signing_key"]
    )
    _serve_this_actors_site(nest, actor)

    try:
        _publish(nest, actor, "Keeper Post", "keeper")
        unpublished_id = _publish(
            nest, actor, "Unpublished Post", "to-unpublish", offset_us=1_000_000
        )
        deleted_id = _publish(
            nest, actor, "Deleted Post", "to-delete", offset_us=2_000_000
        )

        # Baseline: all three really serve, or the withdrawals below prove
        # nothing.
        _assert_serves(nest, "keeper", "Keeper Post", "baseline")
        _assert_serves(nest, "to-unpublish", "Unpublished Post", "baseline")
        _assert_serves(nest, "to-delete", "Deleted Post", "baseline")

        # ── The two revoking doors. ──
        ws_api.web_publish_unset(nest["port"], actor, unpublished_id)
        ws_api.delete_post(
            nest["port"],
            actor,
            sign_and_encode_tombstone(
                actor["signing_key"], deleted_id, int(time.time() * 1_000_000)
            ),
        )

        # Gone at once — the revoke's own render has already replaced the site.
        _assert_gone(nest, "to-unpublish", "Unpublished Post", "after unpublish")
        _assert_gone(nest, "to-delete", "Deleted Post", "after delete")
        _assert_serves(nest, "keeper", "Keeper Post", "after the withdrawals")

        # ── The durability claim: the nest really stops and really comes back. ──
        _restart(nest)

        _assert_gone(nest, "to-unpublish", "Unpublished Post", "after a restart")
        _assert_gone(nest, "to-delete", "Deleted Post", "after a restart")
        _assert_serves(nest, "keeper", "Keeper Post", "after a restart")
    finally:
        _clear_apex(nest)


@pytest.mark.feature("personal-website")
def test_a_blanked_site_comes_back_at_the_next_boot(durable_nest):
    """A site the nest took dark after a failed rebuild renders itself again at
    the next boot, with nothing for the author to do.

    The dark state is produced through the production writer, not by hand:
    `/api/v1/test/web/blank-site` calls
    `CacheDb::clear_web_rendered_owing_restore` — the clear and the owed-restore
    row in one transaction — which is the same call a revoking door whose render
    errors makes (`rerender_fail_closed`). The author then does **nothing at
    all**: no publish, no unpublish, no template sync. Only the boot drain is
    left to pay the debt.

    The **boot-drain half** is what this witnesses, per the file docstring: the
    hook deliberately does not wake the paced restore retry, and nothing outside
    the process can make a render fail, so the retry stays pinned in Rust
    (`web_content::service::start_restore_retry`).
    """
    from conftest import _bridge_admin_post

    nest = durable_nest
    actor = create_actor_and_register(
        nest["port"], admin_signing_key=nest["admin"]["signing_key"]
    )
    _serve_this_actors_site(nest, actor)

    try:
        _publish(nest, actor, "Site That Goes Dark", "dark-site-post")
        _assert_serves(nest, "dark-site-post", "Site That Goes Dark", "baseline")

        # The front page is part of the site, so it goes and comes back with it.
        status, body, _headers = _get(nest["url"], "/")
        assert status == 200 and "Site That Goes Dark" in body, (
            f"the front page must list the published post first: {status} {body}"
        )

        # ── Dark, through the production clear. ──
        blanked = _bridge_admin_post(
            nest["url"],
            nest["admin"]["token"],
            "/api/v1/test/web/blank-site",
            {"actor_id": actor["actor_id_hex"]},
        )
        assert blanked.get("ok"), f"the nest refused to blank the site: {blanked}"

        status, body = _page(nest, "dark-site-post")
        assert status != 200 or "Site That Goes Dark" not in body, (
            f"the site must really be dark before the restore proves anything: {status} {body}"
        )
        status, body, _headers = _get(nest["url"], "/")
        assert BUILTIN_404_MARKER in body or "Site That Goes Dark" not in body, (
            f"a blanked site's front page must be gone too: {status} {body}"
        )

        # The author does nothing. The nest restarts.
        _restart(nest)

        # ── The owed restore is paid by the boot drain, whole. ──
        _assert_serves(nest, "dark-site-post", "Site That Goes Dark", "after a restart")
        status, body, _headers = _get(nest["url"], "/")
        assert status == 200 and "Site That Goes Dark" in body, (
            "the boot drain must restore the WHOLE site, front page included: "
            f"{status} {body}"
        )

        # And the debt is settled, not re-paid forever: a second restart finds
        # nothing owed and the site still up.
        _restart(nest)
        _assert_serves(nest, "dark-site-post", "Site That Goes Dark", "after a second restart")
    finally:
        _clear_apex(nest)
