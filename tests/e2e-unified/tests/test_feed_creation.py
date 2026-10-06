"""Feed creation tests — verify rule parity and combination modes."""
import uuid
import time
import pytest

from tests.api import ws_api

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


@pytest.mark.feature("custom-feeds")
def test_create_feed_body_contains(logged_in_app):
    """Create a feed with a BodyContains rule and verify it FILTERS posts.

    ⚠ **Until this fix the assertion never read a single post.** It created a
    matching and a non-matching post, then asserted only `feed_count >= 1` —
    that a `feed-item` row exists in the sidebar list, which any successful
    feed creation produces regardless of whether the rule filters anything at
    all. A feed whose BodyContains rule silently matched nothing, or matched
    everything, passed identically (
    the same shape as the trending fix: a green assertion is not
    coverage). The fix follows the method: seed a matching post beside a
    non-matching one, prove both render on the unfiltered feed first (so the
    negative assertion below cannot pass vacuously), then require the
    non-matching post to be ABSENT once the ruled feed is selected.
    """
    keyword = _unique("searchword")
    feed_name = _unique("test-feed")
    matching_text = f"post with {keyword} inside"
    other_text = _unique("unrelated-post")

    feed = logged_in_app.feed
    feed.create_post(text=matching_text)
    feed.create_post(text=other_text)

    # Precondition, on the default (unfiltered) feed: BOTH posts are visible
    # here. Without this the negative assertion below could pass vacuously —
    # a post that never rendered in the first place also "is absent".
    assert feed.wait_for_post_text(matching_text), (
        f"post {matching_text!r} should be visible before the ruled feed is "
        f"created; error={logged_in_app.error_text()!r}"
    )
    assert feed.wait_for_post_text(other_text), (
        f"post {other_text!r} should be visible before the ruled feed is "
        f"created; error={logged_in_app.error_text()!r}"
    )

    feed.create_feed_with_rule(
        name=feed_name,
        rule_type="BodyContains",
        value=keyword,
    )
    feed.open_feed(feed_name)

    # The discriminator: a BodyContains rule that actually filters must show
    # the matching post and hide the non-matching one.
    assert feed.wait_for_post_text(matching_text), (
        f"post {matching_text!r} should render in the BodyContains feed "
        f"{feed_name!r}; error={logged_in_app.error_text()!r}"
    )
    assert feed.wait_for_post_text_absent(other_text), (
        f"post {other_text!r} is STILL rendered in the BodyContains feed "
        f"{feed_name!r} — the rule is not filtering; "
        f"error={logged_in_app.error_text()!r}"
    )


@pytest.mark.feature("custom-feeds")
def test_create_feed_has_hashtag(logged_in_app):
    """Create a feed with a HasHashtag rule and verify it FILTERS posts.

    Same fix as :func:`test_create_feed_body_contains`: the old
    assertion checked only that a `feed-item` row existed, never that the
    rule filtered anything. A tagged post beside an untagged one gives the
    rule something to discriminate on.
    """
    tag = _unique("testtag")
    feed_name = _unique("tag-feed")
    tagged_text = _unique("tagged")
    untagged_text = _unique("untagged")

    feed = logged_in_app.feed
    feed.create_post_with_tags(text=tagged_text, tags=tag)
    feed.create_post(text=untagged_text)

    assert feed.wait_for_post_text(tagged_text), (
        f"post {tagged_text!r} should be visible before the ruled feed is "
        f"created; error={logged_in_app.error_text()!r}"
    )
    assert feed.wait_for_post_text(untagged_text), (
        f"post {untagged_text!r} should be visible before the ruled feed is "
        f"created; error={logged_in_app.error_text()!r}"
    )

    feed.create_feed_with_rule(
        name=feed_name,
        rule_type="HasHashtag",
        value=tag,
    )
    feed.open_feed(feed_name)

    assert feed.wait_for_post_text(tagged_text), (
        f"post {tagged_text!r} should render in the HasHashtag feed "
        f"{feed_name!r}; error={logged_in_app.error_text()!r}"
    )
    assert feed.wait_for_post_text_absent(untagged_text), (
        f"post {untagged_text!r} is STILL rendered in the HasHashtag feed "
        f"{feed_name!r} — the rule is not filtering; "
        f"error={logged_in_app.error_text()!r}"
    )


@pytest.mark.feature("custom-feeds")
def test_create_feed_rule_builder_input_widget_switches_on_rule_type(logged_in_app):
    """The create-feed rule-builder's input widget follows the shared
    `RuleInputKind` catalog (`ruleTypeOptions` over wasm — `docs/goal/ui/feed.md`
    § Where logic lives -> Feed rule-builder presentation), not a
    per-rule-type `{#if}` ladder. `test_create_feed_body_contains`/
    `test_create_feed_has_hashtag` already exercise the `Text` kind
    end-to-end; this covers the two kinds neither does — `Toggle`
    (HasMedia/IsReply) and `Number` (MinReplies/MinReposts/CreatedAfter) — by
    driving the widget switch directly. (The required/excluded label TEXT
    itself — `ruleRequiredLabel` — is Rust-unit-tested; this only proves the
    Svelte template shows the right widget for the right kind.)"""
    driver = logged_in_app.driver
    driver.click("feed-create-feed-button")
    driver.wait_for("feed-create-feed-name")

    def _poll_visible(element_id: str, expected: bool, deadline_s: float = 10.0) -> bool:
        # `select()` triggers a reactive show/hide that isn't necessarily
        # committed by the time the very next `is_visible()` fires — a bare
        # instantaneous check flagged this test as a NEW red on 2026-08-02
        # (macOS apple baseline) that reproduced once and passed clean on
        # re-run. Deadline-poll for the target state, same convention-14
        # hardening as test_feed_search.py's clear/by_tag tests, in either
        # direction (poll-until-matches, not just poll-until-visible).
        deadline = time.time() + deadline_s
        visible = driver.is_visible(element_id)
        while time.time() < deadline and visible != expected:
            time.sleep(0.3)
            visible = driver.is_visible(element_id)
        return visible

    def _widget_state(*element_ids: str) -> str:
        """Every input this switch can render, plus the apple registry's own
        reason for each verdict — so one failing run says WHICH half broke.

        A bare `diagnose` of the element that stayed visible cannot distinguish
        the two candidate causes, and they call for opposite fixes: the select
        never took effect (no branch switched — the whole widget set is
        unchanged), versus the branch switched but the *removed* input's
        registry slot is a zombie (the pooled-cell class rule 6 exists for,
        which an iOS `Form` produces and a macOS `VStack` does not). The apple
        `/tree` dump labels each slot `VISIBLE(geo)` / `HIDDEN(geo-parked)` /
        `HIDDEN(votes)` / `VISIBLE(no-geo)`, which is exactly that distinction —
        and reading it is what the media-page saga's three wrong diagnoses cost
        three sessions to learn (apple-e2e-automation.md § Rediagnosis session).
        Empty on every non-apple driver, which simply falls back to `diagnose`.
        """
        parts = [driver.diagnose(e) for e in element_ids]
        # Walk the WHOLE dump, never a pre-filtered slice: slot lines are
        # indented continuations of the id header above them, so filtering
        # headers out first would silently re-parent some other id's slots
        # under ours.
        keep, current = [], False
        for line in driver.tree().splitlines():
            if line.startswith("  ["):
                if current:
                    keep.append(line)
            else:
                current = line.split(" ", 1)[0] in element_ids
                if current:
                    keep.append(line)
        if keep:
            parts.append("registry:\n" + "\n".join(keep))
        return " | ".join(parts)

    # HasMedia -> Toggle: value input hidden, required toggle shown.
    driver.select("feed-rule-type-select", "HasMedia")
    assert not _poll_visible("feed-rule-value-input", False), (
        "Toggle kind (HasMedia) should hide the value input. If the required "
        "toggle IS visible below, the branch switched and the value input's "
        "slot is a zombie (rule 6 / pooled cell); if it is NOT, the select "
        "never took effect. "
        f"{_widget_state('feed-rule-value-input', 'feed-rule-required-toggle')}"
    )
    assert _poll_visible("feed-rule-required-toggle", True), (
        f"Toggle kind (HasMedia) should show the required toggle: "
        f"{driver.diagnose('feed-rule-required-toggle')}"
    )

    # MinReplies -> Number: value input shown (as the count field), no toggle.
    driver.select("feed-rule-type-select", "MinReplies")
    assert _poll_visible("feed-rule-value-input", True), (
        f"Number kind (MinReplies) should show the value input: "
        f"{driver.diagnose('feed-rule-value-input')}"
    )
    assert not _poll_visible("feed-rule-required-toggle", False), (
        "Number kind (MinReplies) should not show the required toggle: "
        f"{driver.diagnose('feed-rule-required-toggle')}"
    )

    # This test inspects widgets and never submits, so it must hand the form
    # back closed — otherwise the next test's opening click TOGGLES it shut on
    # macOS and that test fails looking like a create-feed bug. Convention 10:
    # a test does not leak state into the next one.
    logged_in_app.feed.close_create_feed_form()


@pytest.mark.feature("custom-feeds")
def test_feed_add_rule_button_disabled_for_invalid_input(logged_in_app):
    """`feed-add-rule-button` gates on `fauna_client_feed::can_add_rule`, the
    lift of apple's `FeedCreateForm.canAddRule` (`docs/goal/ui/feed.md` §
    Add-rule gating) — apple was the ONLY client that disabled this button for
    an empty/unparseable rule input; the other six let a user stage a junk
    rule the encoder then silently papers over. Covers a `Text` kind
    (BodyContains) and a `Number` kind (MinReplies); the `TextAndNumber`
    kind's threshold half is already proven end-to-end by
    `test_create_feed_label_below_rule`'s real submit."""
    driver = logged_in_app.driver
    driver.click("feed-create-feed-button")
    driver.wait_for("feed-create-feed-name")

    # Text kind (BodyContains): blank value -> disabled, a real value -> enabled.
    driver.select("feed-rule-type-select", "BodyContains")
    driver.wait_for("feed-rule-value-input")
    driver.clear_and_type("feed-rule-value-input", "")
    assert driver.is_disabled("feed-add-rule-button"), (
        "an empty Text-kind rule value must leave feed-add-rule-button "
        f"disabled: {driver.diagnose('feed-add-rule-button')}"
    )
    driver.clear_and_type("feed-rule-value-input", "rust")
    driver.wait_until_enabled("feed-add-rule-button")

    # Number kind (MinReplies): a blank value -> disabled, an integer -> enabled.
    # (Not a *garbage-text* value: web's number-kind field is a native
    # `<input type="number">`, which the browser itself refuses to accept
    # non-numeric keystrokes into — a real user can't type "not-a-number"
    # there either. A blank value is unparseable on every app and needs no
    # per-app-typed widget assumption.)
    driver.select("feed-rule-type-select", "MinReplies")
    driver.wait_for("feed-rule-value-input")
    driver.clear_and_type("feed-rule-value-input", "")
    assert driver.is_disabled("feed-add-rule-button"), (
        "a blank Number-kind rule value must leave "
        f"feed-add-rule-button disabled: {driver.diagnose('feed-add-rule-button')}"
    )
    driver.clear_and_type("feed-rule-value-input", "5")
    driver.wait_until_enabled("feed-add-rule-button")

    # Same convention-10 rule as the widget-switch test above: leave the form
    # closed so the next test's opening click isn't a TOGGLE-shut on macOS.
    logged_in_app.feed.close_create_feed_form()


@pytest.mark.feature("custom-feeds")
def test_create_feed_label_below_rule(logged_in_app, nest_instance, test_user):
    """LabelBelow -> `TextAndNumber`: both the category value input and the
    threshold number field render, and the submitted rule carries both pieces
    packed as `"category:threshold"` (`RuleInputKind::TextAndNumber`'s only
    consumer, alongside LabelAbove) — verified over the real wire since no
    client UI renders a feed's rules back."""
    feed_name = _unique("label-feed")
    # A NON-default threshold on purpose: every app prefills "5", so asserting
    # the default would pass just as well against a form that ignored the field
    # entirely — which is precisely how this test spent months verifying a rule
    # no user could create (the threshold input had no test id anywhere, so the
    # helper never typed one and the add button was permanently disabled).
    logged_in_app.feed.create_feed_with_rule(
        name=feed_name, rule_type="LabelBelow", value="spam", threshold="3",
    )

    port = nest_instance["port"]
    feeds = ws_api.list_feeds(port, test_user)
    matches = [f for f in feeds if f["name"] == feed_name]
    assert matches, f"created feed {feed_name!r} not in fauna.feed.list: {feeds}"
    feed_id = matches[0]["feed_id"]

    got = ws_api.get_feed(port, test_user, feed_id)
    rules = got["rules"]
    # `FilterRule` is an externally-tagged enum on the wire — LabelBelow encodes as
    # {"LabelBelow": {"category": ..., "max_confidence_permille": ...}}
    # (fauna_client_feed::encoder::encode_filter_rule).
    assert rules and "LabelBelow" in rules[0], f"expected a LabelBelow rule, got {rules!r}"
    assert rules[0]["LabelBelow"]["category"] == "spam", (
        f"LabelBelow should carry the typed category: {rules[0]!r}"
    )
    # The half this test claimed to check but never did. The 0–10 UI threshold
    # reaches the wire as per-mille, so the typed "3" must land as 300 — a value
    # unreachable from the "5" prefill, which is what makes this assertion prove
    # the threshold input is genuinely wired rather than merely present.
    assert rules[0]["LabelBelow"]["max_confidence_permille"] == 300, (
        "LabelBelow must carry the typed threshold (3 -> 300 per-mille), not the "
        f"prefill and not a dropped field: {rules[0]!r}"
    )


@pytest.mark.feature("custom-feeds")
def test_create_feed_with_factor_weight_sets_local_composition(
    logged_in_app, nest_instance, test_user
):
    """Factor-weight editor (feed-factor-*): weighting "Engagement" without
    the global-scope toggle lands the entry on the feed's own `composition`
    (content-moderation-and-ranking.md § Composition), not the caller's
    global factor set — verified via the real wire (`fauna.feed.get`), since
    no client UI yet renders a feed's composition back."""
    feed_name = _unique("weighted-feed")

    logged_in_app.feed.create_feed_with_factor(
        name=feed_name, factor_label="engagement", weight="2.0",
    )

    port = nest_instance["port"]
    feeds = ws_api.list_feeds(port, test_user)
    matches = [f for f in feeds if f["name"] == feed_name]
    assert matches, f"created feed {feed_name!r} not in fauna.feed.list: {feeds}"
    feed_id = matches[0]["feed_id"]

    got = ws_api.get_feed(port, test_user, feed_id)
    composition = got.get("composition")
    assert composition == [{"factor": "engagement", "weight_permille": 2000}], (
        f"expected a single engagement@2000 composition entry, got {composition!r}"
    )


@pytest.mark.feature("custom-feeds")
def test_create_feed_with_global_factor_merges_into_factor_set(
    logged_in_app, nest_instance, test_user, request
):
    """Toggling `feed-factor-global-toggle` routes the entry into the
    caller's global factor set (`fauna.feed.factors.set`) instead of the
    feed's own composition, and merges rather than clobbering whatever else
    is already in the caller's global set (a whole-set-overwrite kind, so the
    create-feed submit must read-merge-write, not blind-set)."""
    port = nest_instance["port"]
    factors_client = ws_api._client_for(ws_api._base_url(port), test_user)

    # A uniquely-named marker entry (never used by any other test/feature)
    # standing in for "whatever the caller already had globally" — proves the
    # merge without assuming a pristine baseline shared with other tests.
    marker_factor = _unique("marker-factor")
    pre_existing = factors_client.call("fauna.feed.factors.get", {})["factors"]
    # Put the set back however this test ends. `test_user` is session-scoped and
    # the nest folds the global set into every score-ordered feed the actor
    # reads, so the `engagement` entry this test adds used to outlive it and
    # re-rank every composed feed after it: in the 2026-09-14 and 2026-09-15
    # whole-suite `--app linux` sweeps an engaged post from an earlier module
    # sat on top of `test_trained_topics.py`'s trained-topic
    # feed.
    request.addfinalizer(
        lambda: ws_api.feed_factors_set(port, test_user, pre_existing)
    )
    factors_client.call(
        "fauna.feed.factors.set",
        {"factors": pre_existing + [{"factor": marker_factor, "weight_permille": -1000}]},
    )

    feed_name = _unique("global-weighted-feed")
    # `"engagement"` is the picker's stable KEY, not its display label ("Engagement")
    # — the cross-app `select(id, value)` contract (see `create_feed_with_factor`).
    # This must be the key: the windows FlaUI bridge matches a ComboBoxItem's
    # AutomationProperties.Name EXACTLY (`flaui-bridge/Actions.cs::Select`), so the
    # label raises there. linux normalizes it back onto the key, and web's Playwright
    # select_option tolerates it too — so the capitalized form this test used to pass
    # was green on linux/web while silently blocking the windows leg (verified
    # on-machine 2026-07-09 with a three-way probe).
    logged_in_app.feed.create_feed_with_factor(
        name=feed_name, factor_label="engagement", weight="1.5",
        global_scope=True,
    )

    feeds = ws_api.list_feeds(port, test_user)
    matches = [f for f in feeds if f["name"] == feed_name]
    assert matches, (
        f"created feed {feed_name!r} not in fauna.feed.list "
        f"(error={logged_in_app.error_text()!r}): {feeds}"
    )
    feed_id = matches[0]["feed_id"]

    got = ws_api.get_feed(port, test_user, feed_id)
    assert got.get("composition") is None, (
        "a global-scope-only factor must not also land on the feed's own "
        f"composition, got {got.get('composition')!r}"
    )

    global_factors = factors_client.call("fauna.feed.factors.get", {})
    by_factor = {f["factor"]: f["weight_permille"] for f in global_factors["factors"]}
    assert by_factor.get("engagement") == 1500
    assert by_factor.get(marker_factor) == -1000, (
        "the pre-existing marker factor must survive the merge-not-overwrite"
    )


@pytest.mark.feature("custom-feeds")
def test_a_feed_with_several_rules_requires_all_of_them_or_any_one(
    logged_in_app, nest_instance, test_user
):
    """Two rules — a hashtag and a word — under each combination mode
    (`feed.md` § Feed-rule types: `All`, every rule must match, or `Any`, at
    least one). Four posts cover the truth table: tagged only, word only, both,
    neither. The "any" feed shows the three that match a rule and hides the one
    that matches none; the "all" feed shows only the post matching both. A
    combination the form never sent, or the nest never honoured, reads the same
    for both feeds and fails one of the two halves."""
    tag = _unique("ruletag")
    word = _unique("ruleword")
    tagged_only = _unique("tagged-only")
    word_only = f"{_unique('word-only')} {word}"
    both = f"{_unique('tagged-and-word')} {word}"
    neither = _unique("neither")

    feed = logged_in_app.feed
    feed.create_post_with_tags(text=tagged_only, tags=tag)
    feed.create_post(text=word_only)
    feed.create_post_with_tags(text=both, tags=tag)
    feed.create_post(text=neither)
    # Precondition on the unfiltered feed, so no absence below is vacuous.
    for text in (tagged_only, word_only, both, neither):
        assert feed.wait_for_post_text(text), (
            f"post {text!r} should be visible before the ruled feeds are "
            f"created; error={logged_in_app.error_text()!r}"
        )

    rules = [("HasHashtag", tag), ("BodyContains", word)]
    any_feed = _unique("any-rule-feed")
    all_feed = _unique("all-rules-feed")
    feed.create_feed_with_rules(any_feed, rules, "any")
    feed.create_feed_with_rules(all_feed, rules, "all")

    # What the form sent, read back over the real wire: both rules, and the
    # combination picked. Without this a missing rule and an ignored mode fail
    # below looking identical.
    port = nest_instance["port"]
    saved = {f["name"]: f for f in ws_api.list_feeds(port, test_user)}
    for name, combination in ((any_feed, "any"), (all_feed, "all")):
        assert name in saved, f"feed {name!r} not in fauna.feed.list: {sorted(saved)}"
        got = ws_api.get_feed(port, test_user, saved[name]["feed_id"])
        sent_rules = got["rules"]
        assert len(sent_rules) == 2 and got.get("combination") == combination, (
            f"feed {name!r} should carry both rules under {combination!r}, got "
            f"combination={got.get('combination')!r} rules={sent_rules!r}"
        )

    feed.open_feed(any_feed)
    for text in (tagged_only, word_only, both):
        assert feed.wait_for_post_text(text), (
            f"post {text!r} matches one of the rules, so the any-rule feed "
            f"{any_feed!r} must show it; error={logged_in_app.error_text()!r}"
        )
    assert feed.wait_for_post_text_absent(neither), (
        f"post {neither!r} matches no rule, yet the any-rule feed {any_feed!r} "
        f"shows it; error={logged_in_app.error_text()!r}"
    )

    feed.open_feed(all_feed)
    assert feed.wait_for_post_text(both), (
        f"post {both!r} matches both rules, so the all-rules feed {all_feed!r} "
        f"must show it; error={logged_in_app.error_text()!r}"
    )
    for text in (tagged_only, word_only):
        assert feed.wait_for_post_text_absent(text), (
            f"post {text!r} matches only one rule, yet the all-rules feed "
            f"{all_feed!r} shows it; error={logged_in_app.error_text()!r}"
        )


@pytest.mark.feature("trending")
def test_trending_is_offered_as_a_factor_for_one_feed_or_every_feed(
    logged_in_app, nest_instance, test_user, request
):
    """Trending sits in `feed-factor-select` like any bus factor
    (`trending.md` § The Trending feed), so a user can weight it into a feed of
    their own, or — with the global toggle — into every feed. Selecting it IS
    the offer check: an agent refuses a value its picker never offered
    (convention 11). Both landings are read over the real wire, since no app
    renders a composition back."""
    port = nest_instance["port"]
    pre_existing = ws_api.feed_factors_get(port, test_user)
    # The global set is folded into every score-ordered feed `test_user` reads
    # for the rest of the run — put it back however this test ends.
    request.addfinalizer(lambda: ws_api.feed_factors_set(port, test_user, pre_existing))

    own_feed = _unique("trending-weighted-feed")
    logged_in_app.feed.create_feed_with_factor(
        name=own_feed, factor_label="trending", weight="2.0",
    )
    feeds = ws_api.list_feeds(port, test_user)
    matches = [f for f in feeds if f["name"] == own_feed]
    assert matches, (
        f"created feed {own_feed!r} not in fauna.feed.list "
        f"(error={logged_in_app.error_text()!r}): {feeds}"
    )
    composition = ws_api.get_feed(port, test_user, matches[0]["feed_id"]).get("composition")
    assert composition == [{"factor": "trending", "weight_permille": 2000}], (
        f"the feed's own composition should weight trending ×2, got {composition!r}"
    )

    every_feed = _unique("trending-everywhere-feed")
    logged_in_app.feed.create_feed_with_factor(
        name=every_feed, factor_label="trending", weight="0.5", global_scope=True,
    )
    by_factor = {
        f["factor"]: f["weight_permille"] for f in ws_api.feed_factors_get(port, test_user)
    }
    assert by_factor.get("trending") == 500, (
        "the global toggle should put trending ×0.5 into the caller's global "
        f"factor set, got {by_factor!r}; error={logged_in_app.error_text()!r}"
    )
