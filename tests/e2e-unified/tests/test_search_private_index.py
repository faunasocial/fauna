"""tier_3: four outcomes of the private, on-device search index witnessed
through the real app UI (`docs/features/private-search-index.md` outcomes 4, 5,
6, 8; `docs/goal/behavior/content-index.md` § Goal, § What's indexed, § Where
queries run — per app).

tui first (the lead app); the other native apps register the same shared local
arm, so their legs are marks added here as each is run. Web is a declared
absence on this page (no browser Tantivy — `content-index.md` § Where queries
run).

**The apps are named per test**, because iOS answers each outcome in the
phone's own shape. A phone builds no index (`content-index.md` § Build vs.
query), so every journey whose hit is content the seat under test seeded is a
desktop journey here, and iOS has a twin of each in which a DESKTOP seat of the
same account builds and the phone finds (`alice_builder_seats`): the desktop
restores the phone's conversations from the `__mls` replica and its draft from
`__drafts` at launch, walks the mailbox and the posts, and publishes; the phone
reads the synced replica. The desktop learns of the phone's content only at its
own launch, so each twin gates the launch on the phone's uploads — a rail blob
present, newer and settled — never on a delay. A draft EDITED on the phone
reaches the index only when a builder next launches (`reserved-folders.md`
§ Drafts Sync — propagation is load-on-launch), so outcome 6's twin launches
its desktop seat twice.

**How a hit is PROVEN local.** Three of the kinds used here — a conversation
message, a draft, junk-filtered mail — never reach the nest's search corpus
(`content_fts` holds public posts, profiles and the public bridge corpus only:
`docs/goal/ui/search.md` § The page's wire surface), so any row for them came
from the private index by construction. A POST is in both corpora, so a post's
hit is taken with the nest arm failing (`helpers/search_journeys.py`
`FailingNestArm`): every row that search commits is the local index's.

**Why every seed is a fresh needle.** The nest and the session-scoped test user
are shared with every other test in the run.
"""

from __future__ import annotations

import sqlite3
import time
import uuid

import pytest

from helpers.budgets import MLS_HANDSHAKE_S
from helpers.search_journeys import (
    HELD_SETTLE_S,
    LOCAL_INDEX_S,
    FailingNestArm,
    actor_client,
    discard_new_thread_draft,
    drafts_blob,
    empty_the_page,
    found,
    kinds_in,
    replica_blob,
    reset_search_page,
    rows,
    start_new_thread_draft,
    wait_drafts_uploaded,
    wait_replica_uploaded,
)
from helpers.waiting import wait_until
from i18n.strings import S
from helpers.mail_aliases import add_exact_alias

# The apps are literal marks on each test, never a computed decorator: the
# feature catalog reads them statically (`scripts/features_scan.py`), and a mark
# it cannot see makes a test speak for every column.
pytestmark = [pytest.mark.tier_3]

#: A held search costs a tick of its own (`FailingNestArm.find`), so a local
#: hit taken with the nest failing gets the flush ceiling plus one tick.
HELD_LOCAL_INDEX_S = LOCAL_INDEX_S + HELD_SETTLE_S

#: Where a post's segments rest on the actor's `__index` rail —
#: `fauna_index::paths::segment_path(ContentKind::Post, seq)`.
POST_SEGMENT_PREFIX = "__index/post/"


def _post_segments(nest_instance, test_user) -> set[str]:
    """The post segments on the account's `__index` rail right now."""
    with actor_client(nest_instance, test_user) as ws:
        entries = ws.call("fauna.index.list", {})["entries"]
    return {e["path"] for e in entries if e["path"].startswith(POST_SEGMENT_PREFIX)}


def _publish_post(nest_instance, test_user, body: str) -> None:
    """A public post by the signed-in user. Fixture setup over the wire
    (convention 8(b)) — the posts arm stages it on its next sweep through
    `fauna.posts.list` (`content-index-ingest.md` § Ingest triggers, v1 → the
    posts ruling), the same as a post composed in the app."""
    from tests.api import ws_api
    from tests.api.bare import sign_and_encode_post

    ws_api.create_post(
        nest_instance["port"],
        test_user,
        sign_and_encode_post(
            test_user["signing_key"], int(time.time() * 1_000_000), body, tags=[]
        ),
    )


def _receive_a_conversation_message(app, nest_instance, test_user, body: str) -> str:
    """A message in a conversation the user is in — sent by another account
    and decrypted by this app. `content-index.md` § What's indexed: the user's
    own content includes "group conversations they're in". Returns the
    conversation's channel id.

    The sender is an API-tier throwaway whose one-shot engine mints a real 1:1
    group, a Welcome for this app's key package, and one sealed application
    message — the recipe `test_conversations_real_search.py` proves."""
    from common import create_actor_and_register
    from tests.api import conv_api

    port = nest_instance["port"]
    me = test_user["actor_id_hex"]
    sender = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    wait_until(
        lambda: conv_api.keypackage_count(port, sender, me) > 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "the app never published a key package to be added by",
    )
    key_package = conv_api.keypackage_fetch(port, sender, me)
    assert key_package is not None, "the app's key package should be fetchable"
    channel_id_hex, welcome, envelope = conv_api.mint_group_welcome_with_message(
        bytes(sender["signing_key"]), key_package, body
    )
    # Reach policy: accept the sender as a contact so the Welcome flows under the
    # default mode (per-pair — the session user's own mode is untouched).
    conv_api.accept_contact(port, test_user, sender["actor_id_hex"])
    conv_api.welcome_deliver(port, sender, me, channel_id_hex, welcome)
    wait_until(
        lambda: any(
            t.channel_id_hex == channel_id_hex for t in app.conversations.list_threads()
        ),
        MLS_HANDSHAKE_S,
        diagnose=lambda: "the app never joined the Welcome's group",
    )
    conv_api.channel_send(port, sender, channel_id_hex, envelope)
    wait_until(
        lambda: any(
            t.channel_id_hex == channel_id_hex and body in (t.snippet or "")
            for t in app.conversations.list_threads()
        ),
        MLS_HANDSHAKE_S,
        diagnose=lambda: "the app never decrypted the sender's message",
    )
    return channel_id_hex


def _send_in_the_conversation(app, channel_id_hex: str, body: str) -> None:
    """The user writes into that conversation through the compose bar — the
    message they SEND, which reaches the index when it is sent
    (`content-index-ingest.md` § Ingest triggers, v1 → the own-send ruling);
    before that ruling it stayed unsearchable until the app restarted."""
    conv = app.conversations
    thread = next(t for t in conv.list_threads() if t.channel_id_hex == channel_id_hex)
    conv.open_thread_by_id(thread.thread_id)
    app.driver.clear_and_type("dm-text-field", body)
    app.driver.click("dm-send-button")
    wait_until(
        lambda: any(
            t.channel_id_hex == channel_id_hex and body in (t.snippet or "")
            for t in conv.list_threads()
        ),
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"the sent message never landed; error={app.error_text()!r}",
    )


def _no_results_for(app, query: str) -> bool:
    """Search `query` once; whether the page settled on "no results". Only
    sound when the page showed ROWS before — a stale "no results" from an
    earlier search would otherwise read as this one's."""
    app.search.query(query)
    return app.search.has_no_results()


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.real_conversations
@pytest.mark.feature("private-search-index", "search")
def test_a_conversation_message_a_post_and_a_draft_of_your_own_are_found_by_their_contents(
    real_faunamls_app, nest_instance, test_user
):
    """`private-search-index` outcome 4 — `content-index.md` § Goal ("one
    unified search across all of a user's own content — mail, calendar,
    conversations, posts, files, contacts, drafts"). Each is found by a phrase
    from its contents, and each row says what kind of thing it is and shows the
    matched text (`search` outcome 7, for the private kinds).

    A conversation message counts both ways it is the user's: one received in
    a conversation they are in, and one they SENT there through the compose
    bar — found in the same session, with no relaunch between the send and the
    search (the own-send ruling, `content-index-ingest.md` § Ingest triggers,
    v1)."""
    app = real_faunamls_app
    message, post, draft = (
        S.search_page.badge_message,
        S.search_page.badge_post,
        S.search_page.badge_draft,
    )
    tag = uuid.uuid4().hex[:10]
    message_needle, post_needle, draft_needle = f"msg{tag}", f"post{tag}", f"draft{tag}"
    sent_needle = f"sent{tag}"

    channel = _receive_a_conversation_message(
        app, nest_instance, test_user, f"the {message_needle} minutes from tuesday"
    )
    _send_in_the_conversation(app, channel, f"the {sent_needle} agenda is attached")
    _publish_post(nest_instance, test_user, f"{post_needle} harbour walk photos")
    start_new_thread_draft(app, f"{draft_needle} notes for the landlord")
    try:
        app.search.navigate()
        # Local-only by construction: none is in the nest's search corpus.
        for needle, kind in (
            (message_needle, message),
            (sent_needle, message),
            (draft_needle, draft),
        ):
            wait_until(
                lambda needle=needle, kind=kind: found(app, needle, kind),
                LOCAL_INDEX_S,
                diagnose=lambda needle=needle, kind=kind: (
                    f"the {kind} carrying {needle!r} was never found by search — "
                    f"rows={rows(app)} error={app.error_text()!r}"
                ),
            )

        # A post is in the nest's corpus too: taken with the nest arm failing,
        # on a page emptied first, so the row can only be the local index's.
        empty_the_page(app)
        with FailingNestArm(nest_instance["port"]) as arm:
            hit = wait_until(
                lambda: arm.find(app, post_needle, post),
                HELD_LOCAL_INDEX_S,
                diagnose=lambda: (
                    f"with the nest arm failing, the post carrying {post_needle!r} "
                    f"was never found — the private index does not hold it; "
                    f"rows={rows(app)} error={app.error_text()!r}"
                ),
            )
        assert "harbour" in hit and kinds_in(hit) == {post}, (
            f"the post's row must show its own text and say it is a {post}: {hit!r}"
        )
    finally:
        reset_search_page(app)
        discard_new_thread_draft(app)


@pytest.mark.ios
@pytest.mark.real_conversations
@pytest.mark.feature("private-search-index", "search")
def test_a_phone_finds_its_own_message_post_and_draft_a_desktop_seat_indexed(
    real_faunamls_app, nest_instance, test_user, alice_builder_seats
):
    """`private-search-index` outcome 4 on a phone — the journey above in the
    phone's real shape (`content-index.md` § Build vs. query). The phone
    receives a conversation message, sends one, publishes a post and writes a
    draft; a DESKTOP seat of the same account (`alice_builder_seats`) is then
    launched beside it, restores the conversation from the `__mls` replica
    (`devices.md` § Cross-device MLS group-state sync — own plaintext rides the
    history slice) and the draft from `__drafts`, walks the posts, and indexes
    all of it; the phone finds each by a phrase from its contents, every row
    saying what kind of thing it is (`search` outcome 7, for the private kinds).

    The desktop learns of the phone's content only at its own launch, so the
    launch waits on the phone's uploads — the replica newer and settled, the
    drafts blob moved — never on a delay. Each needle is found on the desktop
    first, so a miss on the phone names the leg that failed: the desktop's
    index, or the phone's replica."""
    phone = real_faunamls_app
    port = nest_instance["port"]
    message, post, draft = (
        S.search_page.badge_message,
        S.search_page.badge_post,
        S.search_page.badge_draft,
    )
    tag = uuid.uuid4().hex[:10]
    message_needle, post_needle, draft_needle = f"msg{tag}", f"post{tag}", f"draft{tag}"
    sent_needle = f"sent{tag}"

    provider_before = replica_blob(nest_instance, test_user, "provider")
    drafts_before = drafts_blob(nest_instance, test_user)
    channel = _receive_a_conversation_message(
        phone, nest_instance, test_user, f"the {message_needle} minutes from tuesday"
    )
    # Read before the send: the settled slice must be newer than this one, so a
    # slice sealed at the join cannot pass for the whole thread.
    history_before = replica_blob(nest_instance, test_user, f"history/{channel}")
    _send_in_the_conversation(phone, channel, f"the {sent_needle} agenda is attached")
    _publish_post(nest_instance, test_user, f"{post_needle} harbour walk photos")
    start_new_thread_draft(phone, f"{draft_needle} notes for the landlord")
    try:
        wait_replica_uploaded(
            nest_instance, test_user, "provider", superseding=provider_before
        )
        wait_replica_uploaded(
            nest_instance, test_user, f"history/{channel}", superseding=history_before
        )
        wait_drafts_uploaded(nest_instance, test_user, superseding=drafts_before)
        desktop, _ = alice_builder_seats.launch()

        private = (
            (message_needle, message),
            (sent_needle, message),
            (draft_needle, draft),
        )
        desktop.search.navigate()
        for needle, kind in private:
            wait_until(
                lambda needle=needle, kind=kind: found(desktop, needle, kind),
                LOCAL_INDEX_S,
                diagnose=lambda needle=needle, kind=kind: (
                    f"the desktop seat never indexed the {kind} carrying {needle!r} "
                    f"it restored from the phone's rails at launch; "
                    f"rows={rows(desktop)} error={desktop.error_text()!r}"
                ),
            )
        phone.search.navigate()
        # Local-only by construction: none is in the nest's search corpus.
        for needle, kind in private:
            wait_until(
                lambda needle=needle, kind=kind: found(phone, needle, kind),
                LOCAL_INDEX_S,
                diagnose=lambda needle=needle, kind=kind: (
                    f"the phone never found the {kind} carrying {needle!r} the "
                    f"desktop indexed; rows={rows(phone)} error={phone.error_text()!r}"
                ),
            )

        # A post is in the nest's corpus too: taken with the nest arm failing,
        # on pages emptied first, so a row can only be the local index's — the
        # desktop's own copy first, then the replica the phone reads.
        empty_the_page(desktop)
        empty_the_page(phone)
        with FailingNestArm(port) as arm:
            wait_until(
                lambda: arm.find(desktop, post_needle, post),
                HELD_LOCAL_INDEX_S,
                diagnose=lambda: (
                    f"with the nest arm failing, the desktop seat never found the "
                    f"post carrying {post_needle!r} — it never indexed it; "
                    f"rows={rows(desktop)} error={desktop.error_text()!r}"
                ),
            )
            hit = wait_until(
                lambda: arm.find(phone, post_needle, post),
                HELD_LOCAL_INDEX_S,
                diagnose=lambda: (
                    f"with the nest arm failing, the phone never found the post "
                    f"carrying {post_needle!r} in the index the desktop published; "
                    f"rows={rows(phone)} error={phone.error_text()!r}"
                ),
            )
        assert "harbour" in hit and kinds_in(hit) == {post}, (
            f"the post's row must show its own text and say it is a {post}: {hit!r}"
        )
    finally:
        reset_search_page(phone)
        discard_new_thread_draft(phone)


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.real_conversations
@pytest.mark.feature("private-search-index")
def test_content_indexed_on_one_device_is_found_from_another_that_never_indexed_it(
    logged_in_app, nest_instance, test_user, request
):
    """`private-search-index` outcome 5 — `content-index.md` § Goal: the index
    "replicates to all of the user's storage locations".

    Device A indexes a post and publishes it to the actor's `__index` rail. A
    SECOND device of the same account — a fresh state root, never having seen
    the post (`alice_second_device`) — then finds it with the nest arm failing,
    so the hit is its private index's. And it never indexed the post itself:
    the post segments on the rail at the moment it found the post are exactly
    the ones published before it existed. (A resumed builder is seeded from what
    the account already published, `rail_publisher::resume_master_builder`, so
    it has nothing of its own to stage.)

    The seat under test is device A, the BUILDER, so this runs on the apps that
    build; a phone's leg is the next test."""
    def post_segments() -> set[str]:
        return _post_segments(nest_instance, test_user)

    device_a = logged_in_app
    post = S.search_page.badge_post
    needle = f"otherdevice{uuid.uuid4().hex[:10]}"
    _publish_post(nest_instance, test_user, f"{needle} ferry timetable")

    device_a.search.navigate()
    empty_the_page(device_a)
    try:
        with FailingNestArm(nest_instance["port"]) as arm:
            wait_until(
                lambda: arm.find(device_a, needle, post),
                HELD_LOCAL_INDEX_S,
                diagnose=lambda: (
                    f"device A never found its own post {needle!r} in its private "
                    f"index; rows={rows(device_a)} error={device_a.error_text()!r}"
                ),
            )
            published_by_a = post_segments()
            assert published_by_a, "device A's post must rest on the rail as a segment"

            device_b, _ = request.getfixturevalue("alice_second_device")
            device_b.search.navigate()
            hit = wait_until(
                lambda: arm.find(device_b, needle, post),
                HELD_LOCAL_INDEX_S,
                diagnose=lambda: (
                    f"the second device never found {needle!r} through the "
                    f"replicated index; rows={rows(device_b)} "
                    f"error={device_b.error_text()!r}"
                ),
            )
            at_find = post_segments()
        assert at_find == published_by_a, (
            "the second device must find the post through device A's index, "
            "never one it built itself — yet post segments changed between its "
            f"launch and its hit: before={sorted(published_by_a)} "
            f"at the hit={sorted(at_find)}"
        )
        assert "ferry" in hit, f"the hit must show the post's own text: {hit!r}"
    finally:
        reset_search_page(device_a)


@pytest.mark.ios
@pytest.mark.real_conversations
@pytest.mark.feature("private-search-index")
def test_a_phone_finds_content_a_desktop_seat_of_the_same_account_indexed(
    logged_in_app, nest_instance, test_user, alice_builder_seats
):
    """`private-search-index` outcome 5 on a phone — `content-index.md` § Build
    vs. query: "a desktop app … builds and syncs; phones receive the segments
    and query the synced copy".

    The phone (the app under test) is already running. A DESKTOP seat of the
    same account (`alice_builder_seats`) is launched beside it, indexes a post
    and publishes it to the `__index` rail; the phone then finds the post with
    the nest arm failing, so the hit is its private index's, read off the synced
    replica. The phone never built it: the post segments on the rail at the
    phone's hit are exactly the ones the desktop had published."""
    phone = logged_in_app
    post = S.search_page.badge_post
    needle = f"phonefinds{uuid.uuid4().hex[:10]}"
    _publish_post(nest_instance, test_user, f"{needle} ferry timetable")

    phone.search.navigate()
    empty_the_page(phone)
    desktop, _ = alice_builder_seats.launch()
    desktop.search.navigate()
    empty_the_page(desktop)
    try:
        with FailingNestArm(nest_instance["port"]) as arm:
            wait_until(
                lambda: arm.find(desktop, needle, post),
                HELD_LOCAL_INDEX_S,
                diagnose=lambda: (
                    f"the desktop seat never indexed the post {needle!r} — "
                    f"rows={rows(desktop)} error={desktop.error_text()!r}"
                ),
            )
            published = _post_segments(nest_instance, test_user)
            assert published, "the desktop's post must rest on the rail as a segment"

            hit = wait_until(
                lambda: arm.find(phone, needle, post),
                HELD_LOCAL_INDEX_S,
                diagnose=lambda: (
                    f"the phone never found {needle!r} in the index the desktop "
                    f"published; rows={rows(phone)} error={phone.error_text()!r}"
                ),
            )
            at_find = _post_segments(nest_instance, test_user)
        assert at_find == published, (
            "the phone must find the post through the desktop's index, never one "
            "it built — yet post segments changed between the desktop's publish "
            f"and the phone's hit: before={sorted(published)} "
            f"at the hit={sorted(at_find)}"
        )
        assert "ferry" in hit, f"the hit must show the post's own text: {hit!r}"
    finally:
        reset_search_page(phone)


# `real_conversations`: the draft reaches the local index only through the
# builder `start_receive_loop` launches, which windows/macOS/iOS run under e2e
# only with this flag (`test_search_outcomes.py` says the same of its own draft
# journey). Without it the test passed only after a flagged sibling had launched
# the session-cached app first.
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.real_conversations
@pytest.mark.feature("private-search-index")
def test_a_result_shows_the_item_as_it_is_now(logged_in_app):
    """`private-search-index` outcome 6 — `content-index.md` § Where queries
    run: the snippet is the content's CURRENT text, and a hit whose content is
    gone is dropped. Driven on a draft, the kind `content-index.md` names for
    both halves ("the rendered snippet is always the draft's current text, and
    a draft discarded since its segment was sealed resolves to nothing").

    Edited: the new words find it, the row shows them, and the old words stop
    finding it. Deleted: a FRESH search no longer shows it."""
    app = logged_in_app
    draft = S.search_page.badge_draft
    tag = uuid.uuid4().hex[:10]
    old, new = f"draftold{tag}", f"draftnew{tag}"

    start_new_thread_draft(app, f"{old} plan for the move")
    try:
        app.search.navigate()
        wait_until(
            lambda: found(app, old, draft),
            LOCAL_INDEX_S,
            diagnose=lambda: f"the draft {old!r} was never found; rows={rows(app)}",
        )

        # Edit it: re-opening the composer keeps the stashed draft, and the
        # body is replaced whole.
        start_new_thread_draft(app, f"{new} plan for the move")
        app.search.navigate()
        edited = wait_until(
            lambda: found(app, new, draft),
            LOCAL_INDEX_S,
            diagnose=lambda: f"the edited draft {new!r} was never found; rows={rows(app)}",
        )
        assert old not in edited, (
            f"the row must show the draft as it is now, not its old text: {edited!r}"
        )
        # The previous page holds rows, so "no results" can only be this search's.
        wait_until(
            lambda: _no_results_for(app, old),
            LOCAL_INDEX_S,
            diagnose=lambda: (
                f"the draft's old words {old!r} still find it after the edit; "
                f"rows={rows(app)}"
            ),
        )

        # Delete it. Re-find first, so the page holds rows again and the fresh
        # search below cannot be answered by a stale "no results".
        wait_until(lambda: found(app, new, draft), LOCAL_INDEX_S)
        discard_new_thread_draft(app)
        app.search.navigate()
        wait_until(
            lambda: _no_results_for(app, new),
            LOCAL_INDEX_S,
            diagnose=lambda: (
                f"a fresh search still shows the discarded draft {new!r}; "
                f"rows={rows(app)}"
            ),
        )
    finally:
        reset_search_page(app)
        discard_new_thread_draft(app)


@pytest.mark.ios
@pytest.mark.real_conversations
@pytest.mark.feature("private-search-index")
def test_a_phone_result_shows_the_draft_as_it_is_now_once_a_builder_relaunches(
    logged_in_app, nest_instance, test_user, alice_builder_seats
):
    """`private-search-index` outcome 6 on a phone — `content-index.md` § Where
    queries run: the snippet is the content's CURRENT text, and a hit whose
    content is gone is dropped. Driven on a draft the phone writes, indexed by a
    DESKTOP seat of the same account.

    Edited: a desktop learns of another seat's draft only at its own launch
    (`reserved-folders.md` § Drafts Sync — propagation is load-on-launch, never
    push-on-write), so the phone's edit reaches the index when a builder next
    comes online. The first desktop seat is retired and a second launched
    (`alice_builder_seats`); it restores the edited draft, republishes the
    corpus and retires the old segment (the snapshot kind's supersede,
    `content-index-ingest.md` § Ingest triggers, v1 → *Drafts are a snapshot
    kind*): the new words find it, the row shows them, and the old words stop
    finding it. Deleted: the phone resolves a draft hit from its own live store,
    so a FRESH search after the discard shows nothing — no builder involved."""
    phone = logged_in_app
    draft = S.search_page.badge_draft
    tag = uuid.uuid4().hex[:10]
    old, new = f"draftold{tag}", f"draftnew{tag}"

    drafts_before = drafts_blob(nest_instance, test_user)
    start_new_thread_draft(phone, f"{old} plan for the move")
    try:
        settled = wait_drafts_uploaded(nest_instance, test_user, superseding=drafts_before)
        seats = alice_builder_seats
        # `tag=`, not positional: `_BuilderSeats.launch` is a wrapper around the
        # real (seeded) driver launch inside `_alice_extra_seat`, but a single
        # positional string arg matches test_r14_trust_seed_self_launch.py's
        # `_is_driver_launch` shape by coincidence — the keyword is what tells
        # the scanner this is a wrapper call, not a literal `driver.launch(config)`.
        first, _ = seats.launch(tag="alice-builder-seat")
        first.search.navigate()
        wait_until(
            lambda: found(first, old, draft),
            LOCAL_INDEX_S,
            diagnose=lambda: (
                f"the desktop seat never indexed the phone's draft {old!r}; "
                f"rows={rows(first)} error={first.error_text()!r}"
            ),
        )
        phone.search.navigate()
        wait_until(
            lambda: found(phone, old, draft),
            LOCAL_INDEX_S,
            diagnose=lambda: (
                f"the phone never found its draft {old!r} in the desktop's index; "
                f"rows={rows(phone)} error={phone.error_text()!r}"
            ),
        )

        # Edit it on the phone: re-opening the composer keeps the stashed draft,
        # and the body is replaced whole. Then a builder comes online again.
        start_new_thread_draft(phone, f"{new} plan for the move")
        wait_drafts_uploaded(nest_instance, test_user, superseding=settled)
        seats.retire(first)
        second, _ = seats.launch(tag="alice-builder-seat-relaunch")
        second.search.navigate()
        wait_until(
            lambda: found(second, new, draft),
            LOCAL_INDEX_S,
            diagnose=lambda: (
                f"the relaunched desktop seat never indexed the edited draft "
                f"{new!r}; rows={rows(second)} error={second.error_text()!r}"
            ),
        )
        phone.search.navigate()
        edited = wait_until(
            lambda: found(phone, new, draft),
            LOCAL_INDEX_S,
            diagnose=lambda: (
                f"the phone never found the edited draft {new!r} in the relaunched "
                f"desktop's index; rows={rows(phone)} error={phone.error_text()!r}"
            ),
        )
        assert old not in edited, (
            f"the row must show the draft as it is now, not its old text: {edited!r}"
        )
        # The previous page holds rows, so "no results" can only be this search's.
        wait_until(
            lambda: _no_results_for(phone, old),
            LOCAL_INDEX_S,
            diagnose=lambda: (
                f"the draft's old words {old!r} still find it after the relaunched "
                f"builder republished the corpus; rows={rows(phone)}"
            ),
        )

        # Delete it on the phone. Re-find first, so the page holds rows again
        # and the fresh search below cannot be answered by a stale "no results".
        wait_until(lambda: found(phone, new, draft), LOCAL_INDEX_S)
        discard_new_thread_draft(phone)
        phone.search.navigate()
        wait_until(
            lambda: _no_results_for(phone, new),
            LOCAL_INDEX_S,
            diagnose=lambda: (
                f"a fresh search still shows the discarded draft {new!r}; "
                f"rows={rows(phone)}"
            ),
        )
    finally:
        reset_search_page(phone)
        discard_new_thread_draft(phone)


@pytest.fixture
def _spam_model_cleared(nest_instance, test_user):
    """Delete the session user's per-user spam model after the test — the same
    teardown `test_mail_client_spam_receive.py::_isolate_spam_model` runs, for
    its reason: a full-confidence model left on the session-shared user scores
    a token-neutral body as junk and hides a later test's plain mail."""
    yield
    conn = sqlite3.connect(nest_instance["db_path"], timeout=10.0)
    try:
        conn.execute(
            "DELETE FROM spam_models WHERE actor_id = ?1", (test_user["actor_id_bytes"],)
        )
        conn.commit()
    finally:
        conn.close()


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.real_conversations
@pytest.mark.feature("private-search-index")
def test_mail_your_device_filed_as_junk_never_turns_up_in_search(
    logged_in_app, mail_bridge_mta, nest_instance, test_user, _spam_model_cleared,
    seal_helper_binary,
):
    """`private-search-index` outcome 8 — `content-index.md` § What's indexed:
    "On-device-junked mail is never indexed".

    Two messages share one token: one the device's own spam scorer files as
    junk, one it keeps. The kept one is found — so the index has taken in the
    mail that arrived with it — and the junk one is not.

    Delivery and scoring are `test_mail_client_spam_receive.py`'s, reused
    rather than copied."""
    from conftest import _seed_spam_model
    from tests.test_mail_client_spam_receive import (
        HAM_TOKEN,
        SENDER_DOMAIN,
        SPAM_TOKEN,
        _deliver_inbound,
        _junk_watermarked_count,
        _message,
    )

    app = logged_in_app
    actor_id = test_user["actor_id_bytes"]
    db_path = nest_instance["db_path"]
    email = S.search_page.badge_email

    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    domain = mail_bridge_mta.domain
    recipient = add_exact_alias(
        nest_instance["url"], test_user["signing_key"], domain, "junksearchuser"
    )

    # Trained BEFORE delivery, so the pass that drains these already scores them.
    _seed_spam_model(
        db_path=db_path,
        actor_id=actor_id,
        seal_helper_binary=seal_helper_binary,
        ngrams={SPAM_TOKEN: (110, 0), HAM_TOKEN: (0, 110)},
        spam_messages=110,
        ham_messages=110,
    )
    junk_before = _junk_watermarked_count(db_path, actor_id)

    nonce = f"junkidx{uuid.uuid4().hex[:10]}"
    deadline = time.monotonic() + 40.0
    _deliver_inbound(
        mail_bridge_mta.mx_port, domain, recipient,
        _message("External Spammer", recipient, f"Trained spam {nonce}",
                 f"<spam-{nonce}@{SENDER_DOMAIN}>",
                 f"Act now: the {SPAM_TOKEN} offer {nonce} expires today."),
        deadline,
    )
    _deliver_inbound(
        mail_bridge_mta.mx_port, domain, recipient,
        _message("A Colleague", recipient, f"Routine ham {nonce}",
                 f"<ham-{nonce}@{SENDER_DOMAIN}>",
                 f"Thanks for the {HAM_TOKEN} notes {nonce} from the meeting."),
        deadline,
    )

    # The device filed the spam as junk — its disposition reached the nest.
    wait_until(
        lambda: _junk_watermarked_count(db_path, actor_id) > junk_before,
        LOCAL_INDEX_S,
        diagnose=lambda: (
            "the device's spam scorer never filed the spam as junk — without "
            f"that this test proves nothing; error={app.error_text()!r} "
            f"bridge log: {mail_bridge_mta.log_file}"
        ),
    )

    app.search.navigate()
    try:
        # The kept mail is found — the index has taken in this delivery.
        wait_until(
            lambda: found(app, nonce, email),
            LOCAL_INDEX_S,
            diagnose=lambda: (
                f"the kept mail carrying {nonce!r} was never found — the index "
                f"has not taken in this delivery; rows={rows(app)} "
                f"error={app.error_text()!r}"
            ),
        )
        texts = app.search.result_texts()
        spam_marks = ("Trained spam", "Act now", SPAM_TOKEN)
        leaked = [t for t in texts if any(m in t for m in spam_marks)]
        assert not leaked, f"mail filed as junk turned up in search: {leaked!r}"
        assert all(kinds_in(t) == {email} for t in texts if nonce in t), (
            f"every hit for {nonce!r} must be the kept {email}; rows={texts!r}"
        )
    finally:
        reset_search_page(app)


@pytest.mark.ios
@pytest.mark.real_conversations
@pytest.mark.feature("private-search-index")
def test_mail_a_phone_filed_as_junk_never_turns_up_in_its_search_over_a_desktop_seats_index(
    logged_in_app, mail_bridge_mta, nest_instance, test_user, _spam_model_cleared,
    alice_builder_seats, seal_helper_binary,
):
    """`private-search-index` outcome 8 on a phone — `content-index.md` § What's
    indexed: "On-device-junked mail is never indexed", in the phone's real
    shape. Two messages share one token; the phone's own spam scorer files one
    as junk, and its disposition reaches the nest and re-files the mail
    (`mail-spam.md` § Re-file timing). A DESKTOP seat of the same account is
    then launched: it inherits the account's mail key through the `fauna.state.mail` plane,
    re-pages the mailbox from UID 0 (`content-index-ingest.md` § Ingest
    triggers, v1 — the backfill IS the receive walk) and indexes what it keeps;
    the junk never reaches its builder — re-filed before the walk, or suppressed
    by its own scorer before the seam. The phone finds the kept mail over the
    synced index and never the junk.

    Delivery and scoring are `test_mail_client_spam_receive.py`'s, reused
    rather than copied; the phone half of the setup is the journey above's."""
    from conftest import _seed_spam_model
    from tests.test_mail_client_spam_receive import (
        HAM_TOKEN,
        SENDER_DOMAIN,
        SPAM_TOKEN,
        _deliver_inbound,
        _junk_watermarked_count,
        _message,
    )

    phone = logged_in_app
    actor_id = test_user["actor_id_bytes"]
    db_path = nest_instance["db_path"]
    email = S.search_page.badge_email

    phone.mail_settings.navigate()
    phone.mail_settings.ensure_mail_enabled()
    domain = mail_bridge_mta.domain
    recipient = add_exact_alias(
        nest_instance["url"], test_user["signing_key"], domain, "phonejunksearchuser"
    )

    # Trained BEFORE delivery, so the pass that drains these already scores them.
    _seed_spam_model(
        db_path=db_path,
        actor_id=actor_id,
        seal_helper_binary=seal_helper_binary,
        ngrams={SPAM_TOKEN: (110, 0), HAM_TOKEN: (0, 110)},
        spam_messages=110,
        ham_messages=110,
    )
    junk_before = _junk_watermarked_count(db_path, actor_id)

    nonce = f"phonejunkidx{uuid.uuid4().hex[:10]}"
    deadline = time.monotonic() + 40.0
    _deliver_inbound(
        mail_bridge_mta.mx_port, domain, recipient,
        _message("External Spammer", recipient, f"Trained spam {nonce}",
                 f"<spam-{nonce}@{SENDER_DOMAIN}>",
                 f"Act now: the {SPAM_TOKEN} offer {nonce} expires today."),
        deadline,
    )
    _deliver_inbound(
        mail_bridge_mta.mx_port, domain, recipient,
        _message("A Colleague", recipient, f"Routine ham {nonce}",
                 f"<ham-{nonce}@{SENDER_DOMAIN}>",
                 f"Thanks for the {HAM_TOKEN} notes {nonce} from the meeting."),
        deadline,
    )

    # The phone filed the spam as junk — its disposition reached the nest.
    wait_until(
        lambda: _junk_watermarked_count(db_path, actor_id) > junk_before,
        LOCAL_INDEX_S,
        diagnose=lambda: (
            "the phone's spam scorer never filed the spam as junk — without "
            f"that this test proves nothing; error={phone.error_text()!r} "
            f"bridge log: {mail_bridge_mta.log_file}"
        ),
    )

    # A builder comes online: its launch walks the whole mailbox.
    desktop, _ = alice_builder_seats.launch()
    desktop.search.navigate()
    wait_until(
        lambda: found(desktop, nonce, email),
        LOCAL_INDEX_S,
        diagnose=lambda: (
            f"the desktop seat never indexed the kept mail carrying {nonce!r} — "
            f"its mail arm never walked the account's mailbox (no mail key "
            f"through the mail plane?); rows={rows(desktop)} "
            f"error={desktop.error_text()!r}"
        ),
    )

    phone.search.navigate()
    try:
        # The kept mail is found — the desktop's index has reached the phone.
        wait_until(
            lambda: found(phone, nonce, email),
            LOCAL_INDEX_S,
            diagnose=lambda: (
                f"the phone never found the kept mail carrying {nonce!r} in the "
                f"index the desktop published; rows={rows(phone)} "
                f"error={phone.error_text()!r}"
            ),
        )
        texts = phone.search.result_texts()
        spam_marks = ("Trained spam", "Act now", SPAM_TOKEN)
        leaked = [t for t in texts if any(m in t for m in spam_marks)]
        assert not leaked, (
            f"mail the phone filed as junk turned up in its search: {leaked!r}"
        )
        assert all(kinds_in(t) == {email} for t in texts if nonce in t), (
            f"every hit for {nonce!r} must be the kept {email}; rows={texts!r}"
        )
    finally:
        reset_search_page(phone)
