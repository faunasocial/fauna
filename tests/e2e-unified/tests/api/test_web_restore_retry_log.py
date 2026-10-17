"""A restore retry that still fails leaves a warning in the admin's log —
``docs/features/admin-logs.md`` outcome 9, on a real ``fauna-nest`` binary and
the admin's own ``fauna.admin.logs`` read.

``web-content-hosting.md`` § Routing, render, serving owns the promise (*A
blanked site is owed its restore*): a site a fail-closed clear took dark is
re-rendered by the paced **restore retry**, and each pass that leaves a site
dark writes one warning — "one log line per pass is the signal an admin needs".

**How the failure is produced.** A render cannot be made to fail from outside a
running nest, so the ``test-hooks`` build carries
``/api/v1/test/web/render-fault`` (``web_render_fault_test_hook.rs``): it makes
this actor's renders fail and re-renders the site through the production
fail-closed path, whose clear writes the owed-restore row and wakes the retry.
Every later render of the site — the retry's own — fails until the fault is
lifted.

**What this witnesses and what stays Rust-pinned.** The retry's first attempt
after a wake is immediate (``next_attempt_delay`` answers zero with no failure
on record), so the first pass's warning is observable within a test budget and
is what this asserts. The second pass is 900 s out — the retry spreads its
hourly budget of four attempts evenly — so "each pass warns again" stays pinned
in Rust (``web_content::service::start_restore_retry``). The site is brought
back through a real door's render (``publish.set``), never by waiting on the
retry, whose paced sleep a wake cannot shorten.

The nest is a module-scoped **dedicated** nest: the retry's backoff is
nest-wide, so the failure this test records would otherwise pace a sibling
test's restore on the session-shared nest for fifteen minutes.
"""

import time

import pytest

from common.auth import create_actor_and_register
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers import budgets
from helpers.waiting import wait_until

from tests.api import ws_api
from tests.api.bare import sign_and_encode_post
from tests.api.test_web_paywall_folder import _get

pytestmark = pytest.mark.tier_3

RETRY_WARNING = "owed web restores: some blanked sites still fail to render"


@pytest.fixture(scope="module")
def retry_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "nest-web-restore-retry"
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


def _render_fault(nest, actor, fail: bool) -> None:
    from conftest import _bridge_admin_post

    reply = _bridge_admin_post(
        nest["url"],
        nest["admin"]["token"],
        "/api/v1/test/web/render-fault",
        {"actor_id": actor["actor_id_hex"], "fail": fail},
    )
    assert reply.get("ok"), f"the nest refused the render fault (fail={fail}): {reply}"


def _publish(nest, actor, title: str, slug: str) -> None:
    post_bytes = sign_and_encode_post(
        actor["signing_key"], int(time.time() * 1_000_000), title
    )
    post_id = ws_api.create_post(nest["port"], actor, post_bytes)
    ws_api.web_publish_set(nest["port"], actor, post_id, slug)


def _serves(nest, slug: str, marker: str) -> bool:
    status, body, _headers = _get(nest["url"], f"/post/{slug}.html")
    return status == 200 and marker in body


def _pages_down(nest, actor) -> bool:
    client = ws_api._client_for(ws_api._base_url(nest["port"]), actor)
    return client.call("fauna.web.publish.list", {}).get("rendered_pages_down") is True


def _retry_warnings(nest) -> list[dict]:
    with _admin_client(nest) as admin:
        entries = admin.call("fauna.admin.logs", {})["entries"]
    return [e for e in entries if RETRY_WARNING in e["message"]]


@pytest.mark.feature("admin-logs")
def test_a_failing_restore_retry_leaves_a_warning_in_the_nests_log(retry_nest):
    """While the nest keeps failing to bring a member's published pages back,
    the retry's pass leaves a warning the admin reads on the Logs page — and
    the site, still dark, comes back at the next render once rendering works.
    """
    nest = retry_nest
    actor = create_actor_and_register(
        nest["port"], admin_signing_key=nest["admin"]["signing_key"]
    )
    with _admin_client(nest) as admin:
        admin.call(
            "fauna.web.set_apex_actor", {"actor_id": bytes(actor["actor_id_bytes"])}
        )

    try:
        _publish(nest, actor, "Site The Retry Owes", "owed-post")
        assert _serves(nest, "owed-post", "Site The Retry Owes"), "baseline: the page must serve"
        assert not _pages_down(nest, actor), "baseline: a healthy site owes no restore"
        assert not _retry_warnings(nest), "a fresh nest has no failed restore pass to report"

        # ── Renders fail; the fail-closed clear takes the site dark and wakes
        # the retry. ──
        _render_fault(nest, actor, fail=True)
        assert not _serves(nest, "owed-post", "Site The Retry Owes"), (
            "the failed render's clear must take the site dark"
        )

        # ── The retry's first pass fails too, and says so. ──
        warnings = wait_until(
            lambda: _retry_warnings(nest),
            budgets.RESTORE_RETRY_PASS_S,
            diagnose=lambda: f"no {RETRY_WARNING!r} line in fauna.admin.logs; "
            f"restore still owed: {_pages_down(nest, actor)}",
        )
        assert all(w["level"] == "Warn" for w in warnings), warnings

        # The pass that warned left the site dark and its restore still owed.
        assert not _serves(nest, "owed-post", "Site The Retry Owes"), (
            "a pass that warned must not have restored the site"
        )
        assert _pages_down(nest, actor), "the restore must still be owed after a failed pass"

        # ── Rendering works again; the next render any door makes restores the
        # whole site. ──
        _render_fault(nest, actor, fail=False)
        _publish(nest, actor, "After The Fault", "after-post")
        assert _serves(nest, "owed-post", "Site The Retry Owes"), (
            "the next render must bring the blanked page back"
        )
        assert _serves(nest, "after-post", "After The Fault")
        assert not _pages_down(nest, actor), "a restored site must owe nothing"
    finally:
        _render_fault(nest, actor, fail=False)
        with _admin_client(nest) as admin:
            admin.call("fauna.web.set_apex_actor", {"actor_id": None})
