"""tier_3: three Search-page outcomes witnessed through the real app UI —
narrowing to one kind, a failed search that keeps what it found, and what a
result row shows (`docs/features/search.md` outcomes 5, 6, 7;
`docs/goal/ui/search.md` § Goal, § State & data shape, § Errors & edge cases).

tui first (the lead app). The other six apps paint the same shared
`SearchSnapshot` (`libs/fauna-client-search`), so their legs are marks added
here as each is run, never a second copy of these journeys. How a row's kind is
read and how the nest arm is made to fail: `helpers/search_journeys.py`.

Web carries outcomes 5 and 7 only: it has no local arm (`content-index.md`
§ Where queries run), so outcome 6's "keeps what it found" has nothing local to
keep there — an open question for the catalog's gap driver, not this module.
iOS runs 5 and 7 (nest-arm only) as the other apps do. Outcome 6's found half
is a draft this seat wrote, and a phone builds no index of its own
(`content-index.md` § Build vs. query), so iOS has its own witness for it, in
which a DESKTOP seat of the same account indexes the phone's draft
(`alice_builder_seats`) — which is why the apps are named per test.

**Why every seed is a fresh needle.** The nest and the session-scoped test user
are shared with every other test in the run, so a query is only sound on a token
this test minted (`test_search.py`'s own rationale).
"""

from __future__ import annotations

import time
import uuid

import pytest

from helpers.search_journeys import (
    LOCAL_INDEX_S,
    NEST_HIT_S,
    FailingNestArm,
    discard_new_thread_draft,
    drafts_blob,
    empty_the_page,
    found,
    kinds_in,
    kinds_shown,
    reset_search_page,
    rows,
    start_new_thread_draft,
    wait_drafts_uploaded,
)
from helpers.waiting import wait_until
from i18n.strings import S

# The apps are literal marks on each test, never a module mark or a computed
# decorator: the feature catalog reads them statically (`scripts/features_scan.py`),
# and a mark it cannot see makes a test speak for every column.
pytestmark = [pytest.mark.tier_3]

#: The held nest arm is abandoned at the client's 5 s deadline; this is the
#: ceiling on the whole search committing after that, far above a healthy run.
FAILED_SEARCH_SETTLE_S = 30.0


def _seed_a_post_and_a_profile_sharing(needle: str, nest_instance, test_user) -> None:
    """One public post by the signed-in user and one profile whose handle IS the
    needle — two kinds of thing one query finds. Fixture setup over the wire
    (convention 8(b)); the search under test is driven through the app.

    A profile enters the nest's search corpus when an account is admitted under
    a handle (`admin_ws_handlers.rs` → `index_profile`); the post body avoids
    every badge word, so a row's kind can only come from its badge."""
    from nacl.signing import SigningKey

    from common.auth import register_user
    from tests.api import ws_api
    from tests.api.bare import sign_and_encode_post

    post_bytes = sign_and_encode_post(
        test_user["signing_key"],
        int(time.time() * 1_000_000),
        f"{needle} quarterly harbour notes",
        tags=[],
    )
    ws_api.create_post(nest_instance["port"], test_user, post_bytes)

    other = SigningKey.generate()
    register_user(
        nest_instance["port"],
        bytes(other.verify_key).hex(),
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
        handle=needle,
        label=needle,
    )


@pytest.mark.web
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("search")
def test_narrowing_a_search_to_one_kind_shows_only_that_kind(
    logged_in_app, nest_instance, test_user
):
    """`search` outcome 5 — `search.md` § Goal: "a type filter to scope
    results". One query matches a post AND a profile; each filter option must
    then leave exactly its own kind on the page, on the same query, re-fired by
    the filter change alone."""
    app = logged_in_app
    post, profile = S.search_page.badge_post, S.search_page.badge_profile
    needle = f"kinds{uuid.uuid4().hex[:10]}"
    _seed_a_post_and_a_profile_sharing(needle, nest_instance, test_user)

    app.search.navigate()
    try:
        app.search.query(needle)
        # Unfiltered first — both kinds present is the precondition that gives
        # "only one kind" below something to remove.
        wait_until(
            lambda: {post, profile} <= kinds_shown(app),
            NEST_HIT_S,
            diagnose=lambda: (
                f"an unfiltered search for {needle!r} should show a {post} row and "
                f"a {profile} row; rows={rows(app)} error={app.error_text()!r}"
            ),
        )

        app.search.set_type_filter("post")
        # The previous page (both kinds) stays painted until the narrowed search
        # commits, so "only Post" cannot be read off it: the wait is on the
        # committed state, never on a delay.
        wait_until(
            lambda: kinds_shown(app) == {post},
            NEST_HIT_S,
            diagnose=lambda: (
                f"narrowed to posts, only {post} rows should remain; "
                f"rows={rows(app)} error={app.error_text()!r}"
            ),
        )

        app.search.set_type_filter("profile")
        wait_until(
            lambda: kinds_shown(app) == {profile},
            NEST_HIT_S,
            diagnose=lambda: (
                f"narrowed to profiles, only {profile} rows should remain; "
                f"rows={rows(app)} error={app.error_text()!r}"
            ),
        )
    finally:
        reset_search_page(app)


@pytest.mark.web
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("search")
def test_each_search_result_shows_its_matching_snippet_and_its_kind(
    logged_in_app, nest_instance, test_user
):
    """`search` outcome 7 — `search.md` § Goal ("unified result cards") and
    § State & data shape (a row carries its badge and its cleaned snippet).

    EVERY row is checked, not the first: each must carry exactly one kind and a
    snippet of the text that matched — the profile's matched on its NAME, so
    its snippet must be the name, not its empty bio. The post's snippet must
    carry more of its body than the query itself, or a row echoing the search
    box back would pass."""
    app = logged_in_app
    post, profile = S.search_page.badge_post, S.search_page.badge_profile
    needle = f"cards{uuid.uuid4().hex[:10]}"
    _seed_a_post_and_a_profile_sharing(needle, nest_instance, test_user)

    app.search.navigate()
    try:
        app.search.query(needle)
        wait_until(
            lambda: {post, profile} <= kinds_shown(app),
            NEST_HIT_S,
            diagnose=lambda: (
                f"a search for {needle!r} should find the post and the profile; "
                f"rows={rows(app)} error={app.error_text()!r}"
            ),
        )

        texts = app.search.result_texts()
        for i, text in enumerate(texts):
            kinds = kinds_in(text)
            assert len(kinds) == 1, (
                f"row {i} must say what kind of thing it is — exactly one badge; "
                f"found {sorted(kinds)} in {text!r}"
            )
            assert needle in text, (
                f"row {i} must show a snippet of the text that matched "
                f"{needle!r}; got {text!r}"
            )
        post_rows = [t for t in texts if kinds_in(t) == {post}]
        assert any("harbour" in t for t in post_rows), (
            f"the post row's snippet must be the post's own text around the "
            f"match, not the query echoed back; post rows={post_rows!r}"
        )
    finally:
        reset_search_page(app)


# `real_conversations`: the draft reaches the local index only through the
# builder `start_receive_loop` launches, and windows/macOS/iOS run that loop
# under e2e only with this flag (linux and tui run it on every login, so it is a
# no-op there) — `_apply_real_conversations_env`. The same mark
# `test_search_private_index.py` carries for its own draft journey.
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.real_conversations
@pytest.mark.feature("search")
def test_a_failed_search_says_so_and_keeps_what_it_found(logged_in_app, nest_instance):
    """`search` outcome 6 — `search.md` § State & data shape → *The local/nest
    merge* ("A failed nest arm sets `error` and keeps the local rows — partial
    results are shown honestly, never blanked") and § Errors & edge cases.

    The found half is an unsent draft: it lives only on this device and in its
    private index, never in the nest's search corpus, so only the local arm can
    find it. The failing half is the nest arm, made to fail for real — its
    reply held until the client's own deadline abandons it.

    The rows are kept honest by state, not timing: the page is emptied by a
    search that matches nothing, with no error, BEFORE the nest is made to
    fail — so an error beside a needle row can only be the failing search's
    own commit (`helpers/search_journeys.py` `FailingNestArm` says why the
    held request's arrival cannot be read instead)."""
    app = logged_in_app
    port = nest_instance["port"]
    draft = S.search_page.badge_draft
    needle = f"failsoft{uuid.uuid4().hex[:10]}"

    start_new_thread_draft(app, f"{needle} agenda for the offsite")
    try:
        app.search.navigate()

        # 1. The private index holds the draft — found with both arms healthy.
        wait_until(
            lambda: found(app, needle, draft),
            LOCAL_INDEX_S,
            diagnose=lambda: (
                f"the draft carrying {needle!r} was never found by search — the "
                f"local index never served it; rows={rows(app)} "
                f"error={app.error_text()!r}"
            ),
        )

        # 2. Empty the page, so every row after this is the failing search's own.
        empty_the_page(app)
        assert not app.has_error(), (
            f"precondition: nothing has failed yet; error={app.error_text()!r}"
        )

        # 3. The same search again, with the nest arm failing.
        with FailingNestArm(port):
            app.search.query(needle)
            wait_until(
                lambda: app.has_error() and app.search.result_count() > 0,
                FAILED_SEARCH_SETTLE_S,
                diagnose=lambda: (
                    "with the nest arm failing, the page must say the search "
                    "failed AND keep the draft the local arm found: "
                    f"error={app.error_text()!r} rows={rows(app)} "
                    f"no-results={app.search.has_no_results()}"
                ),
            )
            assert S.search_page.search_failed in app.error_text(), (
                f"the page must say the search failed; error={app.error_text()!r}"
            )
            kept = [t for t in app.search.result_texts() if needle in t]
            assert kept and all(kinds_in(t) == {draft} for t in kept), (
                f"the failed search must still show the draft it found; "
                f"rows={rows(app)}"
            )
    finally:
        reset_search_page(app)
        discard_new_thread_draft(app)


@pytest.mark.ios
@pytest.mark.real_conversations
@pytest.mark.feature("search")
def test_a_failed_search_on_a_phone_keeps_the_draft_a_desktop_seat_indexed(
    logged_in_app, nest_instance, test_user, alice_builder_seats
):
    """`search` outcome 6 on a phone — the journey above in the phone's real
    shape. A phone builds no index of its own (`content-index.md` § Build vs.
    query), so the found half — an unsent draft this phone wrote — is indexed by
    a DESKTOP seat of the same account (`alice_builder_seats`), which restores
    the phone's draft from `__drafts` at launch (`reserved-folders.md` § Drafts
    Sync — load-on-launch, so the launch waits on the phone's upload, never on
    a delay) and publishes it; the phone reads the synced replica. The failing
    half is the phone's own nest arm, made to fail for real. The rows are kept
    honest by state exactly as above: the page is emptied, with no error,
    BEFORE the nest is made to fail."""
    phone = logged_in_app
    port = nest_instance["port"]
    draft = S.search_page.badge_draft
    needle = f"phonefailsoft{uuid.uuid4().hex[:10]}"

    drafts_before = drafts_blob(nest_instance, test_user)
    start_new_thread_draft(phone, f"{needle} agenda for the offsite")
    try:
        wait_drafts_uploaded(nest_instance, test_user, superseding=drafts_before)
        desktop, _ = alice_builder_seats.launch()
        desktop.search.navigate()
        wait_until(
            lambda: found(desktop, needle, draft),
            LOCAL_INDEX_S,
            diagnose=lambda: (
                f"the desktop seat never indexed the phone's draft {needle!r} it "
                f"restored at launch; rows={rows(desktop)} "
                f"error={desktop.error_text()!r}"
            ),
        )
        phone.search.navigate()

        # 1. The synced index holds the draft — found with both arms healthy.
        wait_until(
            lambda: found(phone, needle, draft),
            LOCAL_INDEX_S,
            diagnose=lambda: (
                f"the phone never found its draft {needle!r} in the index the "
                f"desktop published; rows={rows(phone)} error={phone.error_text()!r}"
            ),
        )

        # 2. Empty the page, so every row after this is the failing search's own.
        empty_the_page(phone)
        assert not phone.has_error(), (
            f"precondition: nothing has failed yet; error={phone.error_text()!r}"
        )

        # 3. The same search again, with the nest arm failing.
        with FailingNestArm(port):
            phone.search.query(needle)
            wait_until(
                lambda: phone.has_error() and phone.search.result_count() > 0,
                FAILED_SEARCH_SETTLE_S,
                diagnose=lambda: (
                    "with the nest arm failing, the phone must say the search "
                    "failed AND keep the draft the local arm found: "
                    f"error={phone.error_text()!r} rows={rows(phone)} "
                    f"no-results={phone.search.has_no_results()}"
                ),
            )
            assert S.search_page.search_failed in phone.error_text(), (
                f"the page must say the search failed; error={phone.error_text()!r}"
            )
            kept = [t for t in phone.search.result_texts() if needle in t]
            assert kept and all(kinds_in(t) == {draft} for t in kept), (
                f"the failed search must still show the draft it found; "
                f"rows={rows(phone)}"
            )
    finally:
        reset_search_page(phone)
        discard_new_thread_draft(phone)
