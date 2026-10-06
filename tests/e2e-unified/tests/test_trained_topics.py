"""Trainable topic factors — the full S7 loop (topic-factors.md § Goal).

Create a trained topic on the Personalization home → compose a feed dominated
by it → tap *more like this* on a post → the composed feed re-ranks → the
example marker and the sealed model survive an app restart.

tier_3 throughout: a real ``fauna-nest`` binary, real posts, and a real
``order=score`` fetch. The sealed-compose seam re-ranks only pages the nest
actually served — ``inject_posts`` replaces the snapshot and bypasses the seam
entirely, so it must never stand in for the re-rank legs here.

Rollout: linux first (user 2026-07-07), then web (S8, 2026-07-12), windows
(S8, 2026-07-13), macos/ios (S8, 2026-07-15); android's publish-sheet tests
(the two `_publishes_as_a_*` outcomes and the two error-precedence
regressions) carry the `android` marker — built, Robolectric-verified, no
restart involved. The restart test below is the one
exception, left unmarked: it drives `app.driver.hard_reload()`, which
delegates to `PlatformDriver.recover()` for a real force-quit+relaunch —
android is the one driver that has not overridden `recover()`
(`HttpBridgeDriver.supports_cold_relaunch()` is False for it today), so marking it would silently no-op the test's own
restart step rather than prove persistence.
"""
import time
import uuid

import cbor2
import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from tests.api import ws_api

pytestmark = pytest.mark.tier_3

# Distinctive on-topic vocabulary — the trained n-grams must not collide with
# the off-topic posts' words, so the trained post's model score separates.
CAT_TEXT = "tabby kitten purring whiskers softly by the warm window"


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _wait_sample_count(port: int, actor: dict, factor: str, minimum: int,
                       timeout: float = 20.0) -> dict:
    """Poll ``fauna.personalization.model.fetch`` until ``sample_count``
    reaches ``minimum`` — the deterministic gate that the client's
    seal-and-put landed nest-side (the draft-persistence pattern: never
    reload while the write may still be in flight)."""
    deadline = time.monotonic() + timeout
    reply = ws_api.personalization_model_fetch(port, actor, factor)
    while reply.get("sample_count", 0) < minimum and time.monotonic() < deadline:
        time.sleep(0.3)
        reply = ws_api.personalization_model_fetch(port, actor, factor)
    return reply


def _labeler_ws(nest_instance, actor: dict) -> WsRpcAdminClient:
    """A WS-RPC seam for the catalog reads that VERIFY a publish.

    Read-only by construction: `fauna.labelers.{list,inspect}` are the
    inspect-before-subscribe surface any User-class actor may call. The publish
    itself is never driven from here — it is a human act with a client-only
    surface (e2e convention 8).
    """
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(actor["actor_id_bytes"]),
        signing_key=bytes(actor["signing_key"]),
    )


def _list_labelers(nest_instance, actor: dict) -> list:
    """Every catalog row (nest-global — `fauna.labelers.list` has no per-actor
    filter, so a published row is located by diffing, never by publisher)."""
    with _labeler_ws(nest_instance, actor) as ws:
        return ws.call("fauna.labelers.list", {}).get("labelers", [])


def _inspect_labeler(nest_instance, actor: dict, labeler_id: bytes) -> bytes:
    """The raw artifact bytes — `wasm_bytes` carries the List's dag-cbor for the
    `list` kind (frame § Tier-3: "inspect returns the raw artifact")."""
    with _labeler_ws(nest_instance, actor) as ws:
        reply = ws.call("fauna.labelers.inspect", {"labeler_id": labeler_id})
    return reply["wasm_bytes"]


def _wait_top_post_contains(app, needle: str, timeout: float = 15.0) -> str:
    """Poll until the top rendered post's text contains ``needle`` (the
    manager re-ranks asynchronously after a train: delta → re-seal → put →
    re-rank → notify → row rebuild)."""
    deadline = time.monotonic() + timeout
    top = app.feed.first_post_text()
    while needle not in top and time.monotonic() < deadline:
        time.sleep(0.3)
        top = app.feed.first_post_text()
    return top


def _wait_post_order(app, earlier: str, later: str, timeout: float = 20.0):
    """Poll until both texts are rendered, then return their ``(earlier, later)``
    positions in the composed feed.

    The wait is for *both posts to be present* — a latency-independent state —
    and the ORDER is what the caller asserts on afterwards (convention 14): a
    poll that returned as soon as the order happened to hold would green on a
    transient mid-re-rank frame. Returns ``None`` for whichever is missing when
    the budget runs out, so the caller's message can say which.
    """
    def find(needle: str):
        return next(
            (
                i
                for i in range(app.feed.post_count())
                if needle in (app.feed.post_text(i) or "")
            ),
            None,
        )

    deadline = time.monotonic() + timeout
    a, b = find(earlier), find(later)
    while (a is None or b is None) and time.monotonic() < deadline:
        time.sleep(0.3)
        a, b = find(earlier), find(later)
    return a, b


def _as_factor_map(factors: list) -> dict:
    return {f["factor"]: f["weight_permille"] for f in factors}


@pytest.fixture(autouse=True)
def _rank_by_this_modules_factors_alone(nest_instance, test_user):
    """Every journey here asserts what a trained factor does to ORDER, so the
    session actor's global factor set must not be part of the sum.

    The nest folds the caller's global factor set into every score-ordered
    feed, on top of the feed's own composition
    (`bins/fauna-nest/src/feed_routes.rs`, the `order=score` branch). Empty, a
    composition of only sealed factors keys every post at 0 nest-side, which is
    the flat key these journeys are built on (`topic-factors.md` § Scoring).
    With an `engagement` entry an earlier module left in it, an engaged post
    keys above zero and a trained post may never reach the top: step 5 of the
    re-rank journey failed exactly that way in the 2026-09-14 and 2026-09-15
    whole-suite `--app linux` sweeps.

    Emptied for each test and put back afterwards, then read again, so a
    restore that did not take fails here instead of leaking.
    """
    port = nest_instance["port"]
    before = ws_api.feed_factors_get(port, test_user)
    if before:
        ws_api.feed_factors_set(port, test_user, [])
    yield
    if _as_factor_map(ws_api.feed_factors_get(port, test_user)) != _as_factor_map(before):
        ws_api.feed_factors_set(port, test_user, before)
        assert _as_factor_map(ws_api.feed_factors_get(port, test_user)) == _as_factor_map(
            before
        ), "the session actor's global factor set did not restore"


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
# iOS re-enabled 2026-07-17: FeedListView was converted from a lazy
# multi-section `List` to an eager `ScrollView { VStack }`, so every
# post-card now mounts regardless of scroll position — the off-screen
# non-registration blocker this test's step 5 (open_post_actions) hit should no
# longer reproduce.
@pytest.mark.feature("trained-topics")
def test_trained_topic_train_reranks_and_marker_survives_reload(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    port = nest_instance["port"]

    # ── 1. Create the trained topic on the Personalization home ──────────
    app.personalization.navigate()
    topic_name = _unique("Cats")
    app.personalization.create_sole_topic(topic_name)
    assert topic_name in app.personalization.topic_name(0)
    factor_key = app.personalization.topic_factor_key()
    assert factor_key.startswith("topic:") and len(factor_key) == 6 + 32, factor_key

    # A fresh factor has no model row nest-side (absent ⇒ fresh inert model).
    fetch = ws_api.personalization_model_fetch(port, test_user, factor_key)
    assert fetch["sample_count"] == 0 and not fetch.get("sealed_blob")

    # ── 2. Seed real posts: the on-topic post FIRST (strictly oldest, so the
    # flat-key `created_at DESC` order puts it LAST pre-train — a re-rank to
    # the top is then unmistakable) ───────────────────────────────────────
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.create_post(text=CAT_TEXT)
    # sleep-ok: nest `created_at` has EPOCH-SECOND resolution, so two posts are
    # strictly ordered only once the wall clock crosses a second boundary. No
    # deadline poll can substitute: the thing being waited for is the clock.
    time.sleep(1.2)  # sleep-ok: strictly-older created_at (epoch-seconds resolution)
    app.feed.create_post(text=f"quarterly budget spreadsheet {_unique('rows')}")
    app.feed.create_post(text=f"garage door spring replacement {_unique('note')}")

    # ── 3. Compose a feed dominated by the trained factor ────────────────
    feed_name = _unique("cats-feed")
    app.feed.create_feed_with_factor(
        name=feed_name, factor_label=factor_key, weight="5.0",
    )
    feeds = ws_api.list_feeds(port, test_user)
    matches = [f for f in feeds if f["name"] == feed_name]
    assert matches, f"created feed {feed_name!r} not in fauna.feed.list: {feeds}"
    got = ws_api.get_feed(port, test_user, matches[0]["feed_id"])
    assert got.get("composition") == [
        {"factor": factor_key, "weight_permille": 5000}
    ], f"composition did not land: {got.get('composition')!r}"

    # ── 4. Open the composed feed; pre-train the on-topic post ranks LAST
    # (every nest-side key is 0 — the zero-term seam — so order is pure
    # created_at DESC until a sealed contribution separates them) ─────────
    app.feed.open_feed(feed_name)
    deadline = time.monotonic() + 15
    while app.feed.post_count() < 3 and time.monotonic() < deadline:
        time.sleep(0.3)
    count = app.feed.post_count()
    assert count >= 3, f"composed feed rendered {count} posts; error={app.error_text()!r}"
    target_index = next(
        (i for i in range(count) if CAT_TEXT in (app.feed.post_text(i) or "")), None
    )
    assert target_index is not None, "on-topic post missing from the composed feed"
    assert target_index > 0, (
        "pre-train the on-topic post must not already lead (it is the oldest)"
    )

    # ── 5. Train: open the ⋯ menu, confirm the unmarked state, tap *more
    # like this* (train-in-context — the composition's single topic factor).
    # State reads happen with the menu OPEN: a closed popover's children are
    # unmapped and invisible to the element finder. ───────────────────────
    app.feed.open_post_actions(index=target_index)
    assert app.feed.train_verb_state("more") == "off"
    app.feed.click_train_verb("more")

    # Deterministic nest-side gate: the sealed put landed.
    reply = _wait_sample_count(port, test_user, factor_key, 1)
    assert reply["sample_count"] == 1, f"train never landed: {reply!r}"
    assert reply.get("sealed_blob"), "trained model must persist a sealed blob"

    # The loaded window re-ranks: the trained post rises to the top.
    top = _wait_top_post_contains(app, CAT_TEXT)
    assert CAT_TEXT in top, (
        f"composed feed did not re-rank after training; top post = {top!r}, "
        f"error={app.error_text()!r}"
    )
    # The marker renders checked on the (now top-ranked) post. The train's
    # notify rebuilt the rows, so no stale menu is open — open post 0's.
    app.feed.open_post_actions(index=0)
    assert app.feed.train_verb_state("more") == "on"

    # ── 6. Restart the app: the sealed model (and its in-blob marker map)
    # re-hydrates from the nest; the composed feed re-ranks on load ───────
    app.driver.hard_reload()
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.open_feed(feed_name)
    top = _wait_top_post_contains(app, CAT_TEXT, timeout=25)
    assert CAT_TEXT in top, (
        f"trained ranking did not survive the restart; top post = {top!r}, "
        f"error={app.error_text()!r}"
    )
    app.feed.open_post_actions(index=0)
    assert app.feed.train_verb_state("more") == "on", (
        "the example marker (in-blob marker map) must survive the restart"
    )

    # The Personalization facet reflects the advisory example count.
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    assert "1" in app.personalization.topic_example_count_text(0)

    # ── 7. UI-driven delete: the row's Delete button clears the registry
    # entry AND the paired nest-side model row (topic-factors.md § Delete
    # semantics — the pairing was unit-proven; this is the first e2e to
    # CLICK it with a trained model actually present) ─────────────────────
    app.personalization.delete_topic(0)
    assert app.personalization.wait_for_topic_count(0) == 0, (
        f"deleted trained-topic row still renders; error={app.error_text()!r}"
    )
    deadline = time.monotonic() + 10
    gone = ws_api.personalization_model_fetch(port, test_user, factor_key)
    while gone.get("sealed_blob") and time.monotonic() < deadline:
        time.sleep(0.3)
        gone = ws_api.personalization_model_fetch(port, test_user, factor_key)
    assert not gone.get("sealed_blob"), (
        f"the paired fauna.personalization.model.delete never landed: {gone!r}"
    )


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("trained-topics", "community-labelers")
def test_trained_factor_publishes_as_a_pruned_list_labeler(
    logged_in_app, nest_instance, test_user
):
    """Publish a trained factor as a tier-3 List (topic-factors.md § Publishing
    a trained factor; frame D8, user-ratified 2026-07-12).

    The whole voluntary act, through the UI a user actually has: train a
    factor, open the review-prune sheet from its row, uncheck an exemplar the
    user would rather not endorse, name the list publicly, publish. Then prove
    at the wire what landed — the artifact carries the chosen name, exactly the
    kept exemplars, and **no attribution to the publishing actor**.

    The mutation is driven entirely through the client UI (e2e convention 8):
    publishing is a human act with a client-only surface, so an
    ``fauna.labelers.publish`` shortcut here would green while the real sheet
    is broken. The wire reads are verification only.

    linux, web, macOS, and iOS landed 2026-07-16/17; windows follows here.
    android is built (Robolectric-verified) but its e2e bridge is
    host-emulator-gated fleet-wide (topic-factors.md § Implementation status
    today), same as every other android e2e track — not specific to this test.
    """
    app = logged_in_app
    port = nest_instance["port"]

    # ── 1. A trained factor with a real model ────────────────────────────
    app.personalization.navigate()
    topic_name = _unique("PublishCats")
    app.personalization.create_sole_topic(topic_name)
    factor_key = app.personalization.topic_factor_key()

    # ── 2. Seed public posts, then LOAD them into the feed window. The
    # corpus is the loaded window (§ Publishing — the accepted limitation,
    # not a paging crawl), so a feed that was never opened scores nothing:
    # this navigation is load-bearing setup, not cosmetics. ───────────────
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.create_post(text=CAT_TEXT)
    app.feed.create_post(text=f"quarterly budget spreadsheet {_unique('rows')}")

    feed_name = _unique("publish-feed")
    app.feed.create_feed_with_factor(name=feed_name, factor_label=factor_key, weight="5.0")
    app.feed.open_feed(feed_name)
    deadline = time.monotonic() + 15
    while app.feed.post_count() < 2 and time.monotonic() < deadline:
        time.sleep(0.3)
    assert app.feed.post_count() >= 2, (
        f"composed feed never loaded a corpus; error={app.error_text()!r}"
    )

    # Train the on-topic post so the factor actually separates it.
    target = next(
        (i for i in range(app.feed.post_count()) if CAT_TEXT in (app.feed.post_text(i) or "")),
        None,
    )
    assert target is not None, "on-topic post missing from the composed feed"
    app.feed.open_post_actions(index=target)
    app.feed.click_train_verb("more")
    reply = _wait_sample_count(port, test_user, factor_key, 1)
    assert reply["sample_count"] == 1, f"train never landed: {reply!r}"

    # ── 3. Catalog state BEFORE publishing. The publish identity is a
    # per-factor DERIVED keypair (§ Publishing — pseudonymous), so the test
    # cannot know the labeler_id up front; it diffs the nest-global catalog.
    before = {bytes(row["labeler_id"]) for row in _list_labelers(nest_instance, test_user)}

    # ── 4. The review-prune sheet ────────────────────────────────────────
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.open_publish_sheet(0)

    # ≥2, not ≥1: the prune leg below unticks one and still expects a publish,
    # and the submit button is deliberately insensitive with nothing kept. Both
    # seeded posts are public and in the loaded window, so this holds.
    scored = app.personalization.wait_for_publish_exemplars(2)
    assert scored >= 2, (
        f"publish sheet scored {scored} exemplars from the loaded window, need ≥2 "
        f"(both seeded posts are public); error={app.error_text()!r}"
    )

    # The two mandated disclosures (§ Publishing): a List scores only content
    # the publisher saw, and publishing carries no author attribution. Both are
    # accepted limitations the doc obliges the sheet's copy to state.
    note = app.personalization.publish_limitation_text().lower()
    assert note, "the publish sheet must state its limitation copy"
    assert "saw" in note or "seen" in note or "loaded" in note, (
        f"limitation copy must state the corpus limit; got {note!r}"
    )
    assert "anonym" in note or "no author" in note or "not linked" in note, (
        f"limitation copy must state that publishing is anonymous; got {note!r}"
    )

    # Best-scoring first: the trained post leads.
    assert CAT_TEXT[:20] in app.personalization.publish_exemplar_text(0), (
        f"trained post must lead the exemplars; got "
        f"{app.personalization.publish_exemplar_text(0)!r}"
    )
    assert app.personalization.publish_exemplar_score(0).strip(), "score must render"

    # Every exemplar defaults to included (the restore-kind-checkbox shape).
    assert all(app.personalization.publish_exemplar_included(i) for i in range(scored)), (
        "exemplars must default to included — the user PRUNES, never opts each in"
    )

    # ── 5. Prune the last exemplar, name the list, publish ───────────────
    pruned_index = scored - 1
    pruned_text = app.personalization.publish_exemplar_text(pruned_index)
    app.personalization.set_publish_exemplar_included(pruned_index, False)
    assert not app.personalization.publish_exemplar_included(pruned_index)
    kept = scored - 1

    # Unticking everything must disarm Publish rather than no-op a click: an
    # empty List means nothing, and a click that does nothing and says nothing
    # is indistinguishable from a broken sheet.
    for i in range(scored):
        app.personalization.set_publish_exemplar_included(i, False)
    assert not app.personalization.publish_submit_enabled(), (
        "Publish must be disarmed when the user has kept nothing"
    )
    for i in range(scored):
        app.personalization.set_publish_exemplar_included(i, True)
    app.personalization.set_publish_exemplar_included(pruned_index, False)
    assert app.personalization.publish_submit_enabled(), (
        "Publish must re-arm once something is kept again"
    )

    public_name = _unique("Small orange cats")
    app.personalization.type_publish_name(public_name)
    app.personalization.submit_publish()
    assert app.personalization.wait_for_publish_sheet_closed(), (
        f"publish sheet never closed; error={app.error_text()!r}"
    )

    # ── 6. What landed on the registry ───────────────────────────────────
    deadline = time.monotonic() + 15
    new_rows = []
    while time.monotonic() < deadline:
        new_rows = [
            r for r in _list_labelers(nest_instance, test_user)
            if bytes(r["labeler_id"]) not in before
        ]
        if new_rows:
            break
        time.sleep(0.3)
    assert len(new_rows) == 1, (
        f"expected exactly one newly published labeler, got {len(new_rows)}; "
        f"error={app.error_text()!r}"
    )
    row = new_rows[0]
    assert row["artifact_kind"] == "list", f"published as {row['artifact_kind']!r}, not a List"
    assert row["content_kind"] == "post", "a List is public-posts-only (frame § Tier-3)"
    assert row["version"] == 1, f"first publish must be version 1, got {row['version']}"

    # Pseudonymity (§ Publishing, user-ratified): the artifact is signed by a
    # per-factor derived keypair, so it must NOT be attributable to the actor.
    assert bytes(row["publisher_actor"]) != bytes(test_user["actor_id_bytes"]), (
        "a published List must carry no attribution to the publishing actor — "
        "it is signed by the per-factor derived keypair"
    )

    # ── 7. Inspect returns the raw artifact: the name rides INSIDE it, and
    # the entries are exactly what the user kept ─────────────────────────
    artifact = cbor2.loads(bytes(_inspect_labeler(nest_instance, test_user, bytes(row["labeler_id"]))))
    assert artifact.get("name") == public_name, (
        f"the publisher-chosen name must ride inside the artifact; got "
        f"{artifact.get('name')!r}"
    )
    entries = artifact["entries"]
    assert len(entries) == kept, (
        f"published {len(entries)} entries; the user kept {kept} of {scored} "
        f"(pruned {pruned_text!r})"
    )
    for e in entries:
        assert len(bytes(e["content_id"])) == 32, "entry content_id must be a 32-byte digest"
        assert 0 <= e["score"] <= 1000, f"per-mille score out of range: {e['score']}"
    ids = [bytes(e["content_id"]) for e in entries]
    assert ids == sorted(ids), "the artifact must be canonical — strictly ascending content_id"

    # ── 7b. The catalog UI meets the frame's own claim (content-moderation-
    # and-ranking.md § Tier-3 artifact kinds): the row distinguishes a List
    # from a wasm labeler, and inspect renders the decoded publisher-chosen
    # name + the EXACT id→score map — before any subscribe. Until this leg,
    # both were wire-verified only (the "user-deferred 2026-07-16" gap in
    # topic-factors.md § Implementation status). ──────────────────────────
    app.labeler_catalog.navigate_catalog()
    ui_index = app.labeler_catalog.find_index_by_factor(row["factor"])
    assert ui_index is not None, (
        f"published labeler row never appeared in the catalog UI; "
        f"error={app.error_text()!r}"
    )
    assert app.labeler_catalog.item_kind(ui_index) == "list", (
        f"the catalog row must distinguish a List from a wasm labeler; got "
        f"{app.labeler_catalog.item_kind(ui_index)!r}; error={app.error_text()!r}"
    )
    app.labeler_catalog.inspect(ui_index)
    assert app.labeler_catalog.wait_for_inspect_panel(True), (
        f"inspect panel never opened; error={app.error_text()!r}"
    )
    rendered = app.labeler_catalog.wait_for_inspect_list_entries(kept)
    assert rendered == kept, (
        f"inspect rendered {rendered} entry rows, the artifact carries {kept}; "
        f"error={app.error_text()!r}"
    )
    assert public_name in app.labeler_catalog.inspect_list_name_text(), (
        "inspect must render the decoded publisher-chosen name — it rides "
        "inside the artifact, so inspect is where it first becomes visible; got "
        f"{app.labeler_catalog.inspect_list_name_text()!r}"
    )
    assert str(kept) in app.labeler_catalog.inspect_list_entry_count_text()
    # The EXACT map: every rendered row matches the artifact byte-for-byte —
    # canonical ascending order, per-mille scores verbatim (no rescale).
    for i, e in enumerate(entries):
        assert app.labeler_catalog.inspect_list_entry_id(i) == bytes(e["content_id"]).hex(), (
            f"entry {i} id diverges from the artifact"
        )
        assert app.labeler_catalog.inspect_list_entry_score(i) == str(e["score"]), (
            f"entry {i} score diverges from the artifact"
        )
    metadata_text = app.labeler_catalog.inspect_metadata_text()
    assert "artifact_kind: list" in metadata_text, metadata_text
    assert "verified: true" in metadata_text, (
        f"a genuine published List must verify client-side; {metadata_text!r}"
    )
    app.labeler_catalog.close_inspect()

    # ── 8. Clean up the factor we minted ─────────────────────────────────
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1, (
        f"trained-topic row missing before cleanup; error={app.error_text()!r}"
    )
    # `test_user` is session-scoped, so a leftover topic is not this test's
    # private mess. Every suite opens with `create_sole_topic`, which sweeps one
    # up — so this delete is hygiene; the guard against a test that dies before
    # reaching it lives there.
    # (The published List is deliberately NOT withdrawn — it is a fork-at-publish
    # snapshot on a nest-global catalog, and the sibling suites don't read it.)
    app.personalization.delete_topic(0)
    assert app.personalization.wait_for_topic_count(0) == 0, (
        f"publish test left a trained topic behind; error={app.error_text()!r}"
    )


# Widened from windows-only 2026-08-30 (the wide-parity disposition pass): the
# regression these two pin ORIGINATED in windows' PersonalizationPage — the
# docstrings narrate that history — but the contract they assert is app-generic
# (every step is an action-layer call; the error surface is convention 2's
# universal error element), so they carry the same marker set as this file's
# outcome-1/2 siblings. A red on a newly covered app is a genuine
# error-precedence bug on that app, not a test-portability failure.
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("trained-topics")
def test_publish_name_too_long_error_survives_unrelated_rerender(logged_in_app):
    """Regression for the final whole-branch review's one Important finding
    (2026-07-18, `PersonalizationPage.xaml.cs`): `RenderPage()`'s error
    precedence chain (trained-topics VM error → signal-share VM error →
    machine error → else clear) must not silently clobber an error a
    direct-writer set OUTSIDE that chain — `SubmitPublishAsync`'s failure
    path chief among them, since the publish sheet is inline (non-modal) and
    the page's other controls stay interactive while its error is showing.

    Reproduces the reviewer's exact "reachable through ordinary interaction"
    sequence: (a) submit the publish sheet with a name over
    `MAX_LABELER_LIST_NAME_LEN` (128 chars) — a PURE client-side artifact-
    validation failure (`build_list_artifact` rejects it before ever reaching
    the wire), so this is fully deterministic and needs no nest-side fault
    injection; (b) an UNRELATED gesture that also triggers `RenderPage()` —
    the SAME row's "Learn from my activity" engagement toggle, a plain
    registry write with no error of its own — must not clear the
    still-showing publish error out from under the still-open, still-failed
    sheet.

    Before the fix: the toggle's `RenderPage()` call found all three VM/
    machine error tiers empty (the toggle itself succeeded) and fell to the
    final `else`, which cleared `ErrorBar`/`App.CurrentErrorMessage` even
    though the publish sheet was still open and still failed. After the fix,
    `RenderPage()`'s chain includes a fourth tier (`_pageError`, set only by
    `ShowPageError`/cleared only by `ClearPageError`) below the VM/machine
    tiers, so an unrelated successful render can no longer reclaim it.
    """
    app = logged_in_app

    # ── 1. A trained factor with at least one scoreable post. The factor
    # need not be TRAINED — score_corpus_for_factor scores every loaded
    # public post against whatever model exists (a fresh/absent one scores
    # 0, per FeedManager::score_corpus_for_factor / fetch_model), so a single
    # untrained factor + a single seeded post is enough exemplars to arm
    # Submit. ──────────────────────────────────────────────────────────────
    app.personalization.navigate()
    topic_name = _unique("TooLong")
    app.personalization.create_sole_topic(topic_name)
    factor_key = app.personalization.topic_factor_key()

    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.create_post(text=CAT_TEXT)
    feed_name = _unique("toolong-feed")
    app.feed.create_feed_with_factor(name=feed_name, factor_label=factor_key, weight="5.0")
    app.feed.open_feed(feed_name)
    deadline = time.monotonic() + 15
    while app.feed.post_count() < 1 and time.monotonic() < deadline:
        time.sleep(0.3)
    assert app.feed.post_count() >= 1, (
        f"composed feed never loaded a corpus; error={app.error_text()!r}"
    )

    # ── 2. Open the publish sheet and submit an over-length name ─────────
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.open_publish_sheet(0)
    scored = app.personalization.wait_for_publish_exemplars(1)
    assert scored >= 1, f"publish sheet scored no exemplars; error={app.error_text()!r}"
    app.personalization.type_publish_name("x" * 200)
    app.personalization.submit_publish()

    # The failed submit re-enables Submit and leaves the sheet open, writing
    # the error DIRECTLY (PersonalizationPage.xaml.cs SubmitPublishAsync's
    # FfiPublishListException catch → ShowPageError), never through
    # RenderPage()'s own chain.
    deadline = time.monotonic() + 10
    while not app.has_error() and time.monotonic() < deadline:
        time.sleep(0.2)
    assert app.has_error(), "an over-length publish name must surface an error"
    assert app.personalization.publish_sheet_visible(), (
        "a failed submit must leave the sheet open"
    )
    err_before = app.error_text()
    assert err_before, "the direct-written error must be non-empty"

    # ── 3. An UNRELATED gesture that ALSO re-renders the page — the SAME
    # row's engagement toggle, a plain registry write with no error of its
    # own — must NOT silently clear the still-relevant publish error. The
    # clobber isn't necessarily synchronous with the toggle's OWN RenderPage()
    # call — a LabelerCatalogNotifyObserver tick (the reviewer's own named
    # trigger, alongside "a signal-share toggle, a trained-topics op") can
    # land moments later and re-run the same broken precedence chain — so
    # this holds the assertion over a window rather than reading once. ─────
    app.personalization.set_engagement_toggle(0, True)
    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline:
        assert app.has_error(), (
            "an unrelated successful RenderPage()-triggering gesture (the "
            "engagement toggle, or a later LabelerCatalogNotifyObserver "
            "tick) clobbered the still-relevant publish error"
        )
        assert app.error_text() == err_before, (
            f"the page error changed after an unrelated render: "
            f"{err_before!r} -> {app.error_text()!r}"
        )
        time.sleep(0.3)

    # ── Cleanup — test_user is session-scoped and shared with this module's
    # other tests (test-order load-bearing, per the file's convention) ─────
    app.personalization.cancel_publish()
    app.personalization.delete_topic(0)
    assert app.personalization.wait_for_topic_count(0) == 0, (
        f"regression test left a trained topic behind; error={app.error_text()!r}"
    )


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("trained-topics")
def test_publish_error_clears_on_fresh_sheet_reopen(logged_in_app):
    """Regression for the re-review's ONE remaining gap
    (2026-07-19, `PersonalizationPage.xaml.cs`): `OpenPublishSheetAsync`
    never retired a stale `_pageError` from a PRIOR failed publish attempt —
    neither the top of the method nor the success path
    (`RenderPublishExemplars`) called `ClearPageError`. Reachable through
    ordinary interaction: open the sheet, a transient failure shows an error,
    the user cancels, then re-opens and successfully retries — the stale
    banner from the FIRST attempt survived indefinitely, until an unrelated
    `SubmitPublishAsync`/`DeleteCueRollupAsync` success happened to clear it.

    This is a DIFFERENT target than
    `test_publish_name_too_long_error_survives_unrelated_rerender` above: that
    one proves an UNRELATED re-render (the engagement toggle) must NOT clear
    a still-relevant error; this one proves a FRESH, user-initiated sheet
    RE-OPEN — the start of a new attempt — MUST clear a now-stale one. Reuses
    the sibling test's exact deterministic failure trigger (an over-length
    publish name — a pure client-side `build_list_artifact` validation
    failure, no nest-side fault injection needed) purely to arm `_pageError`;
    genuinely faulting `ScoreCorpusForFactorAsync` itself has no deterministic
    trigger without fault-injecting the nest, so this exercises the same
    `_pageError` field via `SubmitPublishAsync`'s catch instead — the fix
    (`ClearPageError()` at the top of `OpenPublishSheetAsync`) doesn't care
    which direct-writer raised the stale error, only that a fresh open
    retires it.

    Before the fix: re-opening the sheet left the stale error showing
    (nothing clears `_pageError` on open, and a successful corpus score /
    `RenderPublishExemplars` doesn't call `ClearPageError` either) — the
    banner would persist through this entire cancel → reopen → successful
    retry sequence. After the fix, the reopen itself clears it immediately,
    before the new corpus score even lands.
    """
    app = logged_in_app

    # ── 1. A trained factor with a composed feed carrying a scoreable corpus
    # (identical shape to the sibling regression above). ──────────────────
    app.personalization.navigate()
    topic_name = _unique("StaleErr")
    app.personalization.create_sole_topic(topic_name)
    factor_key = app.personalization.topic_factor_key()

    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.create_post(text=CAT_TEXT)
    feed_name = _unique("staleerr-feed")
    app.feed.create_feed_with_factor(name=feed_name, factor_label=factor_key, weight="5.0")
    app.feed.open_feed(feed_name)
    deadline = time.monotonic() + 15
    while app.feed.post_count() < 1 and time.monotonic() < deadline:
        time.sleep(0.3)
    assert app.feed.post_count() >= 1, (
        f"composed feed never loaded a corpus; error={app.error_text()!r}"
    )

    # ── 2. Open the publish sheet and fail it with an over-length name ────
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.open_publish_sheet(0)
    scored = app.personalization.wait_for_publish_exemplars(1)
    assert scored >= 1, f"publish sheet scored no exemplars; error={app.error_text()!r}"
    app.personalization.type_publish_name("x" * 200)
    app.personalization.submit_publish()

    deadline = time.monotonic() + 10
    while not app.has_error() and time.monotonic() < deadline:
        time.sleep(0.2)
    assert app.has_error(), "an over-length publish name must surface an error"
    assert app.personalization.publish_sheet_visible(), (
        "a failed submit must leave the sheet open"
    )

    # ── 3. Cancel WITHOUT retrying — closing the sheet alone must NOT clear
    # the error (only ClearPageError does; ClosePublishSheet doesn't call it),
    # so this isolates the reopen itself as the thing under test next. ─────
    app.personalization.cancel_publish()
    assert not app.personalization.publish_sheet_visible()
    assert app.has_error(), (
        "closing the sheet without retrying must not, by itself, clear the "
        "still-relevant error (that would mask what's actually under test)"
    )

    # ── 4. Re-open the SAME sheet fresh — the gap under test. A fresh,
    # user-initiated open is a new attempt; it must retire the PRIOR
    # attempt's stale error immediately, not leave it hanging until an
    # unrelated success. Polled (not read once) since before the fix this
    # would never clear at all, not just clear slowly. ─────────────────────
    app.personalization.open_publish_sheet(0)
    deadline = time.monotonic() + 5.0
    while app.has_error() and time.monotonic() < deadline:
        time.sleep(0.2)
    assert not app.has_error(), (
        "a fresh sheet re-open must clear the stale error from the prior "
        f"failed attempt; still showing: {app.error_text()!r}"
    )

    # ── 5. Complete a REAL successful retry — a valid short name this time —
    # proving the whole loop recovers cleanly end-to-end, not just the
    # immediate post-reopen clear. ──────────────────────────────────────────
    scored_retry = app.personalization.wait_for_publish_exemplars(1)
    assert scored_retry >= 1, (
        f"retry publish sheet scored no exemplars; error={app.error_text()!r}"
    )
    app.personalization.type_publish_name(_unique("StaleErrOk"))
    app.personalization.submit_publish()
    assert app.personalization.wait_for_publish_sheet_closed(), (
        f"a valid retry submit must close the sheet; error={app.error_text()!r}"
    )
    assert not app.has_error(), (
        f"a successful retry must leave no error showing; got {app.error_text()!r}"
    )

    # ── Cleanup — test_user is session-scoped and shared with this module's
    # other tests (test-order load-bearing, per the file's convention) ─────
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.delete_topic(0)
    assert app.personalization.wait_for_topic_count(0) == 0, (
        f"regression test left a trained topic behind; error={app.error_text()!r}"
    )


# The Model kind's training corpus, as (verb, text) pairs.
#
# BOTH classes are marked, and that is not decoration: the subscriber's scorer
# is a Bernoulli posterior `P(more | text)`, so a corpus with only *more like
# this* examples has nothing to be more likely THAN and scores every post the
# same. Three distinct posts per class also satisfies
# `TEXT_MODEL_PUBLISH_MIN_DOCS = 3` — the class-blind anti-quote privacy floor —
# and each post carries its own filler so they are three genuinely different
# documents rather than three copies (a corpus of clones would pass the floor
# without proving the floor counts DISTINCT documents).
MODEL_MORE = [
    "orange tabby sunning on the porch railing",
    "orange tabby chasing a paper ball down the hall",
    "orange tabby asleep in the laundry basket",
]
MODEL_LESS = [
    "quarterly depreciation schedule for the delivery vans",
    "quarterly depreciation notes ahead of the audit",
    "quarterly depreciation rollup for the finance review",
]
MODEL_CORPUS = [("more", t) for t in MODEL_MORE] + [("less", t) for t in MODEL_LESS]
# Neither is posted before the publish, and neither is ever marked: the whole
# point of a Model over a List is that it generalizes to content the publisher
# never saw. The MISS post shares the *disliked* vocabulary, so the pair proves
# an ordering, not merely that one post scored something.
UNSEEN_MATCH_TEXT = "a stray orange tabby turned up at the shelter this morning"
UNSEEN_MISS_TEXT = "the quarterly depreciation figures are attached for review"


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("trained-topics", "community-labelers", "custom-feeds")
def test_trained_factor_publishes_as_a_scrubbed_text_model(
    logged_in_app, nest_instance, test_user
):
    """Publish a trained factor as a tier-3 `text-model` and prove the one
    thing a List cannot do — it generalizes to an UNSEEN post
    (topic-factors.md § Publishing a trained factor, v2, ratified 2026-08-13;
    the kind's contract: content-moderation-and-ranking.md § Tier-3 artifact
    kinds).

    Every mutation is driven through the app UI (e2e convention 8): publishing
    and subscribing are human acts with a client-only surface, so a
    ``fauna.labelers.publish`` shortcut here would green while the real sheet
    is broken. The wire reads verify only.

    The legs, in order:

    1. train three DISTINCT public posts per class, each class sharing an
       n-gram — below three the privacy floor admits nothing, which is a
       refusal, not an empty list, and a one-class corpus cannot discriminate;
    2. review the whole vocabulary in the sheet's Model body, prune one pattern;
    3. publish, and verify at the wire that the artifact carries the chosen
       name, exactly the kept vocabulary, and no attribution to the actor;
    4. inspect renders that vocabulary in full, before any subscribe;
    5. subscribe, compose the labeler into a feed, and post something the
       factor was never trained on — it re-ranks to the top. That last leg is
       the generalization proof, and it is the reason this kind exists.

    tui leads (the lead-app rule); the other six follow in the batched
    trickle-down.
    """
    app = logged_in_app
    port = nest_instance["port"]

    # ── 1. A factor trained on three distinct public posts ───────────────
    app.personalization.navigate()
    topic_name = _unique("ModelCats")
    app.personalization.create_sole_topic(topic_name)
    factor_key = app.personalization.topic_factor_key()

    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    # Compose onto the chronological feed. `create_post` knows its post landed
    # when the post is the newest card on top, and on a session actor past the
    # 50-post display cap that is its only signal; a ranked feed an earlier test
    # left selected would put an older post above it.
    app.feed.open_feed("General")
    for _verb, text in MODEL_CORPUS:
        app.feed.create_post(text=text)

    feed_name = _unique("model-corpus-feed")
    app.feed.create_feed_with_factor(
        name=feed_name, factor_label=factor_key, weight="5.0"
    )
    app.feed.open_feed(feed_name)
    deadline = time.monotonic() + 30
    while app.feed.post_count() < len(MODEL_CORPUS) and time.monotonic() < deadline:
        time.sleep(0.3)
    assert app.feed.post_count() >= len(MODEL_CORPUS), (
        f"composed feed loaded {app.feed.post_count()} of {len(MODEL_CORPUS)} "
        f"corpus posts; error={app.error_text()!r}"
    )

    # Mark all six. The scrub reads the factor's example MARKERS (ids only) and
    # re-fetches each post, so the training gestures are what build the publish
    # corpus — there is no other door into it.
    for expected, (verb, text) in enumerate(MODEL_CORPUS, start=1):
        target = next(
            (
                i
                for i in range(app.feed.post_count())
                if text in (app.feed.post_text(i) or "")
            ),
            None,
        )
        assert target is not None, (
            f"corpus post missing from the composed feed: {text!r}"
        )
        app.feed.open_post_actions(index=target)
        app.feed.click_train_verb(verb)
        reply = _wait_sample_count(port, test_user, factor_key, expected)
        assert reply["sample_count"] == expected, (
            f"train {expected} of {len(MODEL_CORPUS)} ({verb}) never landed: "
            f"{reply!r}"
        )

    before = {bytes(r["labeler_id"]) for r in _list_labelers(nest_instance, test_user)}

    # ── 2. The Model review body ─────────────────────────────────────────
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.open_publish_sheet(0)
    # The sheet opens on the WEAKER disclosure — a List publishes only ids the
    # publisher already made public, so it is what an unattended default picks.
    assert (
        app.personalization.publish_kind() == app.personalization.PUBLISH_KIND_LIST
    ), (
        f"the sheet must default to List; got "
        f"{app.personalization.publish_kind()!r}"
    )

    app.personalization.set_publish_kind(app.personalization.PUBLISH_KIND_MODEL)
    assert (
        app.personalization.publish_kind() == app.personalization.PUBLISH_KIND_MODEL
    ), f"kind select did not take; error={app.error_text()!r}"
    surviving = app.personalization.wait_for_publish_ngrams(2)
    assert surviving >= 2, (
        f"the scrub produced {surviving} patterns from {len(MODEL_CORPUS)} "
        f"public examples — at least 'orange tabby' and 'quarterly "
        f"depreciation' each appear in three, so both must survive the 3-post "
        f"floor; refusal_visible="
        f"{app.personalization.publish_ngram_refusal_visible()}, "
        f"error={app.error_text()!r}"
    )
    assert not app.personalization.publish_ngram_refusal_visible(), (
        "the refusal state must not paint beside a non-empty vocabulary"
    )

    # The three mandated Model disclosures (§ Publishing — absence of any one
    # is a bug), plus the corpus-size line that makes the rebuild's drops
    # visible.
    note = app.personalization.publish_limitation_text().lower()
    assert note, "the Model sheet must state its limitation copy"
    assert "seen" in note, (
        f"copy must say the model generalizes to content nobody here has seen; "
        f"got {note!r}"
    )
    assert "3" in note or "three" in note, (
        f"copy must disclose the ≥3-post pattern floor; got {note!r}"
    )
    assert "less like this" in note, (
        f"copy must disclose the DISLIKE half — the direction column exists "
        f"because it is part of what crosses; got {note!r}"
    )
    assert "anonym" in note, f"copy must state pseudonymity; got {note!r}"
    assert str(len(MODEL_CORPUS)) in note, (
        f"the corpus-size line must state how many public examples the "
        f"vocabulary was built from; got {note!r}"
    )

    # Every survivor carries the three facts the review is made of, and every
    # one defaults to INCLUDED — the user prunes the scrub's proposal.
    directions = set()
    for i in range(surviving):
        assert app.personalization.publish_ngram_text(i).strip(), (
            f"n-gram row {i} rendered no text"
        )
        direction = app.personalization.publish_ngram_direction(i).strip()
        assert direction, (
            f"n-gram row {i} rendered no class direction — it is part of the "
            f"ratified disclosure, not decoration"
        )
        directions.add(direction)
        assert app.personalization.publish_ngram_count_text(i).strip(), (
            f"n-gram row {i} rendered no distinct-post count"
        )
        assert app.personalization.publish_ngram_included(i), (
            "n-grams must default to included — the user PRUNES, never opts in"
        )
    # Half this corpus was marked *less like this*, so the review must SHOW
    # that half as such: the dislike patterns are part of the disclosure, and a
    # column that read the same for both would hide it.
    assert len(directions) > 1, (
        f"a corpus with both classes must render more than one direction; "
        f"every row said {directions!r}"
    )

    vocabulary = [app.personalization.publish_ngram_text(i) for i in range(surviving)]
    assert "orange tabby" in vocabulary, (
        f"the pattern shared by all three liked examples must survive the "
        f"floor; vocabulary={vocabulary!r}"
    )

    # ── 3. Prune, name, publish ──────────────────────────────────────────
    # Prune something that is neither class's discriminating pattern, so leg 6's
    # generalization proof still has a working vocabulary on both sides.
    load_bearing = {"orange tabby", "quarterly depreciation"}
    pruned_index = next(
        (i for i, n in enumerate(vocabulary) if n not in load_bearing), None
    )
    assert pruned_index is not None, (
        f"expected the scrub to yield more than the two discriminating "
        f"patterns, so a prune leg is possible; vocabulary={vocabulary!r}"
    )
    pruned_text = vocabulary[pruned_index]
    app.personalization.set_publish_ngram_included(pruned_index, False)
    assert not app.personalization.publish_ngram_included(pruned_index)
    kept = surviving - 1

    # Nothing kept must DISARM Publish rather than no-op a click: an empty
    # vocabulary is a refusal the shared lifecycle enforces anyway, and a click
    # that does nothing and says nothing reads as a broken sheet.
    for i in range(surviving):
        app.personalization.set_publish_ngram_included(i, False)
    assert not app.personalization.publish_submit_enabled(), (
        "Publish must be disarmed when the user has kept no word patterns"
    )
    for i in range(surviving):
        app.personalization.set_publish_ngram_included(i, True)
    app.personalization.set_publish_ngram_included(pruned_index, False)
    assert app.personalization.publish_submit_enabled(), (
        "Publish must re-arm once something is kept again"
    )

    public_name = _unique("Orange tabbies")
    app.personalization.type_publish_name(public_name)
    app.personalization.submit_publish()
    assert app.personalization.wait_for_publish_sheet_closed(), (
        f"publish sheet never closed; error={app.error_text()!r}"
    )

    # ── 4. What landed on the registry ───────────────────────────────────
    deadline = time.monotonic() + 20
    new_rows = []
    while time.monotonic() < deadline:
        new_rows = [
            r
            for r in _list_labelers(nest_instance, test_user)
            if bytes(r["labeler_id"]) not in before
        ]
        if new_rows:
            break
        time.sleep(0.3)
    assert len(new_rows) == 1, (
        f"expected exactly one newly published labeler, got {len(new_rows)}; "
        f"error={app.error_text()!r}"
    )
    row = new_rows[0]
    assert row["artifact_kind"] == "text-model", (
        f"published as {row['artifact_kind']!r}, not a text-model"
    )
    assert row["content_kind"] == "post", "a text-model is posts-only at v1"
    # Same pseudonymity rule as the List: the per-factor derived keypair signs,
    # so the artifact must not be attributable to the publishing actor.
    assert bytes(row["publisher_actor"]) != bytes(test_user["actor_id_bytes"]), (
        "a published model must carry no attribution to the publishing actor"
    )

    artifact = cbor2.loads(
        bytes(_inspect_labeler(nest_instance, test_user, bytes(row["labeler_id"])))
    )
    assert artifact.get("name") == public_name, (
        f"the publisher-chosen name must ride inside the artifact; got "
        f"{artifact.get('name')!r}"
    )
    assert artifact["version"] == 1, (
        f"v1 is this build's tokenizer contract; got {artifact['version']!r}"
    )
    ngrams = artifact["ngrams"]
    assert len(ngrams) == kept, (
        f"published {len(ngrams)} patterns; the user kept {kept} of "
        f"{surviving} (pruned {pruned_text!r})"
    )
    assert pruned_text not in [n["ngram"] for n in ngrams], (
        f"the pruned pattern {pruned_text!r} crossed anyway"
    )
    texts = [n["ngram"] for n in ngrams]
    assert texts == sorted(texts), (
        "the artifact must be canonical — strictly ascending n-grams"
    )
    for n in ngrams:
        # The privacy floor is STRUCTURAL: the two per-class counts ARE the
        # distinct-document counts, so the nest's own validator refuses a
        # below-floor entry whoever built it.
        assert n["more"] + n["less"] >= 3, (
            f"{n['ngram']!r} crossed below the 3-post anti-quote floor: {n!r}"
        )
        assert n["more"] > 0 or n["less"] > 0, f"all-zero entry: {n!r}"
    # The class doc counters say what the vocabulary was BUILT from, and are
    # not shrunk to match a pruned vocabulary — they are the posterior's priors
    # and the cold-start damp's sample count.
    assert artifact["more_docs"] == len(MODEL_MORE), (
        f"more_docs must count the liked public examples the rebuild used, not "
        f"the kept patterns; got {artifact['more_docs']!r}"
    )
    assert artifact["less_docs"] == len(MODEL_LESS), (
        f"less_docs must count the disliked examples — the dislike half is part "
        f"of what the model learned; got {artifact['less_docs']!r}"
    )

    # ── 5. Inspect renders the FULL vocabulary, before any subscribe ─────
    app.labeler_catalog.navigate_catalog()
    ui_index = app.labeler_catalog.find_index_by_factor(row["factor"])
    assert ui_index is not None, (
        f"published model never appeared in the catalog UI; "
        f"error={app.error_text()!r}"
    )
    assert app.labeler_catalog.item_kind(ui_index) == "text-model", (
        f"the catalog row must distinguish a model from a list/wasm labeler; "
        f"got {app.labeler_catalog.item_kind(ui_index)!r}"
    )
    app.labeler_catalog.inspect(ui_index)
    assert app.labeler_catalog.wait_for_inspect_panel(True), (
        f"inspect panel never opened; error={app.error_text()!r}"
    )
    rendered = app.labeler_catalog.wait_for_inspect_model_entries(kept)
    assert rendered == kept, (
        f"inspect rendered {rendered} vocabulary rows, the artifact carries "
        f"{kept} — a truncated vocabulary is not the artifact; "
        f"error={app.error_text()!r}"
    )
    assert public_name in app.labeler_catalog.inspect_model_name_text(), (
        f"inspect must render the decoded publisher-chosen name; got "
        f"{app.labeler_catalog.inspect_model_name_text()!r}"
    )
    assert str(kept) in app.labeler_catalog.inspect_model_ngram_count_text()
    assert app.labeler_catalog.inspect_model_entry_texts() == texts, (
        "every rendered row must match the artifact, in its canonical order"
    )
    for i in range(kept):
        assert app.labeler_catalog.inspect_model_entry_direction(i).strip(), (
            f"vocabulary row {i} rendered no class direction"
        )
        assert app.labeler_catalog.inspect_model_entry_count(i).strip(), (
            f"vocabulary row {i} rendered no distinct-document count"
        )
    metadata_text = app.labeler_catalog.inspect_metadata_text()
    assert "artifact_kind: text-model" in metadata_text, metadata_text
    assert "verified: true" in metadata_text, (
        f"a genuine published model must verify client-side; {metadata_text!r}"
    )
    app.labeler_catalog.close_inspect()

    # ── 6. THE GENERALIZATION PROOF — an UNSEEN post re-ranks ────────────
    # This is the leg a List cannot give: the post below is created AFTER the
    # publish, was never in the corpus, and was never marked. Only a model that
    # generalizes can rank it.
    app.labeler_catalog.subscribe(ui_index)
    assert app.labeler_catalog.wait_for_subscribed_state(ui_index, subscribed=True), (
        f"subscribe never flipped the row; error={app.error_text()!r}"
    )

    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    # Back to the chronological feed first: leg 1 left its factor-ranked
    # corpus feed selected, where a liked corpus post outranks the new one and
    # `create_post` never sees its post reach the top (see leg 1).
    app.feed.open_feed("General")
    # The MATCH post is created FIRST, so the flat `created_at DESC` order puts
    # the MISS post above it before any scoring. A re-rank that lifts the match
    # to the top is then unmistakable, and cannot be recency in disguise.
    unseen_match = UNSEEN_MATCH_TEXT + " " + _unique("shelter")
    app.feed.create_post(text=unseen_match)
    # sleep-ok: same epoch-second boundary as the seeding step above.
    time.sleep(1.2)  # sleep-ok: strictly-newer created_at (epoch-seconds resolution)
    app.feed.create_post(text=UNSEEN_MISS_TEXT + " " + _unique("ledger"))

    # A feed composed of the SUBSCRIBED LABELER ALONE. The publisher's own
    # sealed tier-1 factor is deliberately not in this composition: if it were,
    # a re-rank would prove nothing about the published artifact.
    model_feed = _unique("subscribed-model-feed")
    app.feed.create_feed_with_factor(
        name=model_feed, factor_label=row["factor"], weight="5.0"
    )
    app.feed.open_feed(model_feed)
    deadline = time.monotonic() + 30
    while app.feed.post_count() < 2 and time.monotonic() < deadline:
        time.sleep(0.3)
    assert app.feed.post_count() >= 2, (
        f"the subscribed-model feed never loaded; error={app.error_text()!r}"
    )
    # The assertion is the ORDER OF THE TWO UNSEEN POSTS, not "something with
    # the right words reached the top" — the corpus posts carry that vocabulary
    # too and would satisfy a top-of-feed check while proving nothing about
    # generalization. Neither post below existed when the model was published,
    # and the disliked one is the NEWER of the two, so recency alone would put
    # it first: only a model that generalizes can invert them.
    # Matched on the CONSTANTS, not the unique-suffixed bodies: the suffix sits
    # at the end, so it is exactly what a truncated card preview drops.
    match_at, miss_at = _wait_post_order(
        app, UNSEEN_MATCH_TEXT, UNSEEN_MISS_TEXT, 30.0
    )
    assert match_at is not None and miss_at is not None, (
        f"both unseen posts must render in the composed feed (match at "
        f"{match_at}, miss at {miss_at}); error={app.error_text()!r}"
    )
    assert match_at < miss_at, (
        f"a subscribed text-model must rank an UNSEEN post carrying its liked "
        f"vocabulary above an unseen post carrying the disliked half — that "
        f"generalization is the entire reason this kind exists, and a List "
        f"could not do it. The liked one rendered at {match_at}, the disliked "
        f"one at {miss_at}, and the disliked one is the NEWER of the two "
        f"(so this is not recency); error={app.error_text()!r}"
    )

    # ── 7. Cleanup — test_user is session-scoped and shared with this
    # module's other tests (test order is load-bearing here) ─────────────
    app.labeler_catalog.navigate_catalog()
    idx = app.labeler_catalog.find_index_by_factor(row["factor"])
    if idx is not None:
        app.labeler_catalog.unsubscribe(idx)
        app.labeler_catalog.wait_for_subscribed_state(idx, subscribed=False)
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1, (
        f"trained-topic row missing before cleanup; error={app.error_text()!r}"
    )
    app.personalization.delete_topic(0)
    assert app.personalization.wait_for_topic_count(0) == 0, (
        f"the model-publish test left a trained topic behind; "
        f"error={app.error_text()!r}"
    )
