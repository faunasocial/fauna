"""An actor switch performed **while the media route is open** must rebind that
page to the incoming actor: their own items list, their own thumbnails paint, and
none of the outgoing actor's decrypted state survives.

The `routes/media` twin of the conversations-page bug closed
and the feed-page bug closed: ``account-scoping.md`` §
The scoping taxonomy → *The switch/sign-out isolation contract* — "no account-scoped
datum may be read or written by a session authenticated as a different account" —
and its in-memory corollary, which names `routes/media`'s owner-key-**decrypted**
thumbnail blob URLs specifically as one of the five surfaces that needed (and got) the `lib/actorScope.ts` seam.

That prior track's own "Residual" section flagged this surface as unpinned: "no
suite switches actors on `/app/media` ... `routes/media`'s `onActorChange` rebuild
in particular has never executed in a test" — and named the missing piece exactly:
"media needs a second actor with readable media, which no fixture builds today."
This module is that fixture (module-local, not a shared conftest fixture — narrow
enough to keep here) plus the switch test itself.

**Why each actor gets a manifest-seeded placeholder BEFORE the real upload.**
Historical origin: at the time this module was written, `MediaSnapshot::folders()`
listed only sets that already **had** media, so a freshly-created empty set had no
`media-folder-filter` option and `upload_selected`'s fallback had nothing to resolve
to — this test's first draft tried `set_filter` on a truly-empty actor and hit a
real 30s Playwright timeout ("did not find some options"), not a product bug. That
mechanism is FIXED (the empty-folder defect, 2026-08-03): `folders()`
(`libs/fauna-client-media/src/lib.rs`) now unions the control-plane set list with
the item-derived one, and the upload fallback is
`s.filter.or_else(|| s.raw.upload_targets().first().cloned())` — an empty Sync set
is both offerable and targetable. The placeholder seeding stays because the
"clean 1 → 2" count shape below still wants a pre-existing member.
`test_media.py::test_media_upload_into_selected_set` does it the
same way every other media fixture does: it seeds via the manifest-only
`_seed_cross_set_media` (`conftest.py`, the real `fauna.sync.changes.record` RPC, no
daemon) FIRST, so the set already has one item before any real upload — the
established "clean 1 → 2" shape. This module follows the same pattern per actor, then
uploads a REAL `>300×300` image as the second member so `thumbnail_hash` is genuine
(the on-device `process_media` producer only runs over real image bytes) and
`painted_thumbnail_count()` observes an actual producer → fetch → owner-key-decrypt
→ paint round trip, not just a manifest row.
"""

import shutil
from pathlib import Path

import pytest

from common.auth import create_actor_and_register

pytestmark = [pytest.mark.tier_3]

FIXTURE_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"

# Generous, latency-independent budget (convention 14): the switch resets the
# page's actor-scoped state and the rebuild is one `fauna.media.list` round trip
# plus a thumbnail fetch+decrypt.
ACTOR_SWITCH_WAIT_S = 30.0


def _make_media_actor(nest_instance, *, set_name: str, placeholder_path: str) -> dict:
    """Register a fresh actor with ONE owned `sync` folder, seeded with a
    manifest-only placeholder member (via `conftest._seed_cross_set_media`, the real
    `fauna.sync.changes.record` RPC) so the set is enumerable via
    `MediaSnapshot::folders()` *before* the real upload below — see the module
    docstring. Fixture setup (e2e point 8(b)), not the mutation under test."""
    from conftest import _seed_cross_set_media

    user = create_actor_and_register(
        nest_instance["port"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    _seed_cross_set_media(nest_instance, user, {set_name: [placeholder_path]})
    return user


def _upload_named_copy(tmp_path: Path, basename: str) -> str:
    """A real copy of the shared fixture image under a distinct basename — the
    recorded member path is the picked file's own basename
    (``apps/fauna-linux/src/views/media/mod.rs``), so two actors uploading
    DIFFERENT basenames record distinguishable members."""
    assert FIXTURE_IMAGE.exists(), f"missing image fixture: {FIXTURE_IMAGE}"
    dest = tmp_path / basename
    shutil.copyfile(FIXTURE_IMAGE, dest)
    return str(dest)


def _switch_to(app, request, nest, user, *, handle: str) -> None:
    """``set_state`` login as ``user`` landing on the media view.

    Naming the route the app is ALREADY on (``media``) is the forcing function:
    a same-route ``goto`` does not remount the page, so any actor-scoped state
    held on the component (``thumbUrls``, ``thumbAttempted``, the poll ``timer``,
    the component-local ``machine``) survives the switch unless
    ``resetActorScopedMedia`` + the ``onActorChange`` rebuild actually run.
    Mirrors ``test_conversations_actor_switch.py``'s ``_switch_to``.
    """
    node_url = request.getfixturevalue("spa_url") if app.driver.is_web() else nest["url"]
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": node_url,
            "secret_hex": bytes(user["signing_key"]).hex(),
            "handle": handle,
            "actor_id": user["actor_id_hex"],
            "device_id": "test-device-media-switch",
        },
        "nav": {"stack": [{"view": "media"}]},
    })


@pytest.mark.web
@pytest.mark.feature("multiple-accounts")
def test_actor_switch_on_the_media_route_rebinds_the_page(
    app, request, nest_instance, tmp_path
):
    """Switching actors while ON /app/media must rebind the page to actor B: their
    own item renders, their own thumbnail paints, and actor A's item is gone.

    Asserts latency-independent state (convention 14): the observable is whose
    items the page can list, deadline-polled to a generous budget.
    """
    from conftest import _login_app_as

    actor_a = _make_media_actor(
        nest_instance, set_name="media-switch-a-set", placeholder_path="placeholder-a.bin"
    )
    actor_b = _make_media_actor(
        nest_instance, set_name="media-switch-b-set", placeholder_path="placeholder-b.bin"
    )
    assert actor_a["actor_id_hex"].lower() != actor_b["actor_id_hex"].lower(), (
        "the two actors must differ"
    )

    a_path = _upload_named_copy(tmp_path, "actor-a-only.png")
    b_path = _upload_named_copy(tmp_path, "actor-b-only.png")

    # ── Precondition: seed actor B's own media FIRST (fixture setup, point 8(b)) ──
    # A separate login on the SAME app/driver, reset before and after — actor A's
    # baseline (below) must not observe B's session.
    _login_app_as(app, request, nest_instance, actor_b)
    app.media.navigate()
    pre_b = app.media.wait_for_item_count(1)
    assert pre_b == 1, (
        f"actor B's placeholder seed did not land: count={pre_b}, "
        f"error={app.error_text()!r}"
    )
    app.media.upload_file(b_path)
    b_count = app.media.wait_for_item_count(2)
    assert b_count == 2, (
        f"actor B's real upload did not land: count={b_count}, "
        f"error={app.error_text()!r}"
    )
    assert "actor-b-only.png" in app.media.item_names(), (
        f"actor B's uploaded item is missing: {app.media.item_names()!r}"
    )
    app.driver.reset()

    # ── Baseline: actor A logs in on /app/media, own item + thumbnail render. ──
    # A red here is a broken baseline, never the regression under test (convention 6).
    _login_app_as(app, request, nest_instance, actor_a)
    app.media.navigate()
    pre_a = app.media.wait_for_item_count(1)
    assert pre_a == 1, (
        f"actor A's placeholder seed did not land: count={pre_a}, "
        f"error={app.error_text()!r}"
    )
    app.media.upload_file(a_path)
    a_count = app.media.wait_for_item_count(2)
    assert a_count == 2, (
        f"actor A's baseline upload did not land: count={a_count}, "
        f"error={app.error_text()!r}"
    )
    assert "actor-a-only.png" in app.media.item_names(), (
        f"actor A's uploaded item is missing: {app.media.item_names()!r}"
    )
    painted_a = app.media.wait_for_painted_thumbnails(1)
    if painted_a is not None:
        assert painted_a == 1, (
            f"actor A's own thumbnail did not paint before the switch: {painted_a}"
        )

    # ── Switch WITHOUT leaving the media route. ────────────────────────────────
    _switch_to(app, request, nest_instance, actor_b, handle="e2e-media-switch-b")

    # ── Actor B's own items must render, with their own painted thumbnail. ─────
    # Pre-fix (no rebind) the page would keep showing actor A's items + thumbnail
    # (or, worse, a revoked/blank thumbnail with A's stale item still listed) —
    # the component-local `machine`/`thumbUrls`/`thumbAttempted` built for A in a
    # one-shot mount that a same-route switch does not re-run.
    b_count_after = app.media.wait_for_item_count(2, timeout=ACTOR_SWITCH_WAIT_S)
    assert b_count_after == 2, (
        f"actor B's own media did not render after an actor switch performed "
        f"while the media route was open — the page is still bound to actor A's "
        f"MediaMachine (a component-local build in `onMount` that never re-ran, "
        f"because a switch is a same-route `goto` and the route does not "
        f"remount). count={b_count_after}, error={app.error_text()!r} "
        f"{app.driver.diagnose('media-item')}"
    )
    assert "actor-b-only.png" in app.media.item_names(), (
        f"the items rendered after the switch are not actor B's own: "
        f"{app.media.item_names()!r} (expected 'actor-b-only.png' among them)"
    )

    # ── ...and actor A's item must be gone. ────────────────────────────────────
    # The isolation contract's other direction: B may not RENDER A's local state.
    assert "actor-a-only.png" not in app.media.item_names(), (
        f"actor A's item is still rendered to actor B after the switch — the "
        f"switch/sign-out isolation contract (account-scoping.md) forbids "
        f"account B rendering account A's local state. "
        f"items={app.media.item_names()!r}"
    )

    painted_b = app.media.wait_for_painted_thumbnails(1, timeout=ACTOR_SWITCH_WAIT_S)
    if painted_b is not None:
        assert painted_b == 1, (
            f"actor B's own thumbnail did not paint after the switch (a stale, "
            f"un-revoked blob URL from actor A would either leave this at 0 or "
            f"paint the wrong image): {painted_b}"
        )
