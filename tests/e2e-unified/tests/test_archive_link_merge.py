"""Phase two of archive import, witnessed end to end: two people who each
imported their Facebook archive link through the Contacts row, and each one's
comments and reactions on the other's imported posts come back signed by the
person who wrote them.

``docs/goal/behavior/archive-import.md`` § Linking and merging (phase two) —
the journey ruling 12 spells out — and § Testing (the tier_3 phase-two
bullet). Two seats on one machine (e2e convention 16): A is
``real_faunamls_app`` (the session's alice), B is ``second_real_faunamls_app``
(a fresh bob, his push arm and ticker muted, so every wait for something to
reach B pokes his receive loop — ``helpers.waiting.poke_receive_cycle``).

**The contact step.** The second-seat fixture already wrote B's accept toward
A by API (it owes B's reachability). With that edge in place a contact request
from A to B delivers straight into B's inbox — no knock row is stored and
nothing writes A's edge (``routes.rs``'s ``allow_knock`` arm: an ``accepted``
sender is delivered). So the one UI gesture that leaves A's roster row
``accepted`` is the other direction: B finds A and sends the request, A's
``allow_knock`` inbox holds it as a knock, and A accepts it on the Contacts
page (``contacts.md`` § Persistence → *The accept makes the edge mutual*).
Both gestures are the app's; A's inbox mode is pinned to its default by API
as an arrangement and put back afterwards (``test_contacts_accept_opens_
conversation.py`` does the same).

**Before the contact (outcome 50).** B's first import announces to every
accepted contact, which includes A; A has no edge toward B, so the nest's
reach gate refuses B's Welcome (or, were it delivered, A's sink would drop a
sender its roster does not list) — either way A is never asked. The witness
is A's Contacts page with no ``contact-archive-link-prompt`` on any row.

**B's view of A's post.** B reads A's link post in his own feed through an
author-granted follow, seeded headless exactly as ``test_archive_import.py``'s
``_granted_by_the_owners_app`` does (an arrangement of who sees whom, not one
of the journey's mutations).

**Isolation.** alice is session-scoped and may import again in a later test of
the session, so this journey's archives name a link of their own
(``FakeFacebookArchive.link``) and its texts carry a tag: two link posts of one
account sharing a link would make ruling 5's rung (iii) refuse as ambiguous.

**Outcome 19's absence.** After the unlink B imports one more new comment and
it must not come back. A merge is B's own post, written by B's run right after
the Done step paints, so there is no later event to anchor "it did not come"
on. The window is calibrated from this very run: outcome 55's positive control
measured how long B's claim took from Done to A's reply count moving, and the
absence is held for three times that (with a floor) — a merge that would still
arrive fails the assertion.
"""

from __future__ import annotations

import dataclasses
import re
import sys
import time
import uuid
from pathlib import Path

import cbor2
import pytest

from actions.api_actor import ApiActor
from actions.archive_import import ArchiveImportActions
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers import budgets
from helpers.contacts_knocks import knock_row, open_people
from helpers.waiting import (
    await_receive_cycle_after,
    conv_receive_cycles,
    poke_receive_cycle,
    wait_until,
)

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "fakes"))
from fake_facebook_archive import (  # noqa: E402
    DEFAULT_POSTS,
    FRIEND_TWO_COMMENT,
    FRIEND_TWO_COMMENT_TS,
    LINK_POST,
    LINK_TEXT,
    SECOND_COMMENT,
    SECOND_COMMENT_TS,
    THIRD_COMMENT,
    FakeFacebookArchive,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

FOLLOWERS_TIER = "followers"  # fauna_core::subscription::FOLLOWERS_TIER
LOVE = "❤️"  # archive-import.md ruling 7: Love → React { emoji: "❤️" }
MICROS_PER_SECOND = 1_000_000
# The Contacts row's five phase-two ids (ui.yaml `contacts`, rule A 2026-11-01).
PROMPT = "contact-archive-link-prompt"
YES = "contact-archive-link-yes"
LINKED = "contact-archive-linked"
UNLINK = "contact-archive-unlink"
ROW = "contact-row[0]"  # the roster narrowed to the one peer (`narrow_roster`)
# The floor under outcome 19's calibrated absence window (see the docstring).
ABSENCE_FLOOR_S = 10.0


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _client(node_url: str, actor: dict) -> WsRpcAdminClient:
    return WsRpcAdminClient(node_url, actor_id=actor["actor_id_bytes"], signing_key=bytes(actor["signing_key"]))


def _call(node_url: str, actor: dict, kind: str, payload: dict) -> dict:
    with _client(node_url, actor) as c:
        return c.call(kind, payload)


# ── the wizard ───────────────────────────────────────────────────────────────


def _import(app, zip_path: Path, owner: str) -> str:
    """Source → Archive → Scope → Confirm → Start → Done, as a user does it;
    returns the Done summary. Leaves the wizard fresh for the next import."""
    wizard = ArchiveImportActions(app.driver)
    wizard.navigate()
    assert wizard.is_page_visible(), f"page error: {wizard.error_text()!r}"
    wizard.select_source("Facebook")
    wizard.next()
    wizard.set_archive_path(str(zip_path))
    wizard.open_archive()
    summary = wait_until(
        lambda: wizard.archive_summary() or None,
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"the archive never indexed; error was {wizard.error_text()!r}",
    )
    assert summary.startswith(f"Facebook · {owner}"), summary
    wizard.next()
    assert wait_until(lambda: wizard.category_labels(), budgets.UI_SETTLE_S,
                      diagnose=lambda: "the Scope step listed no categories")
    wizard.next()
    assert wait_until(lambda: wizard.confirm_summary() or None, budgets.UI_SETTLE_S,
                      diagnose=lambda: "no confirm summary")
    wizard.start()
    done = wait_until(
        lambda: wizard.done_summary() or None,
        budgets.RPC_ROUNDTRIP_S * 3,
        diagnose=lambda: f"the import never reached Done; progress {wizard.progress_summary()!r}, "
        f"skips {wizard.error_log()!r}, error {wizard.error_text()!r}",
    )
    assert "imported" in done, done
    wizard.back()  # Done → a fresh wizard
    return done


# ── the Contacts row ─────────────────────────────────────────────────────────


def _peer_row(app, peer_hex: str) -> None:
    """The Contacts page, People half, the roster narrowed to ``peer_hex`` —
    so ``contact-row[0]`` is that person's row whatever else the actor holds."""
    open_people(app)
    app.contacts.narrow_roster(peer_hex)
    wait_until(
        lambda: app.contacts.contact_count() == 1 or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the roster never narrowed to {peer_hex[:12]}: names {app.contacts.contact_names()!r}, "
            f"error {app.error_text()!r}"
        ),
    )


def _row_has(app, element_id: str) -> bool:
    return app.driver.count(element_id, scope=ROW) > 0


def _wait_row(app, element_id: str, *, poke=None, present: bool = True, what: str) -> None:
    """Wait for ``element_id`` to be (or stop being) on the narrowed row. A
    seat whose receive loop is muted passes ``poke`` — its driver — and every
    poll asks it for a cycle first. The page repaints the rows while it shows
    (`DataMessage::ArchiveLinksMoved`), so the wait stays on Contacts."""

    def check():
        if poke is not None:
            poke_receive_cycle(poke)
        return _row_has(app, element_id) == present or None

    wait_until(
        check,
        budgets.MLS_HANDSHAKE_S,
        interval=2,
        diagnose=lambda: (
            f"{what}: {element_id} {'never appeared' if present else 'never left'} on the row; "
            f"row status {app.contacts.contact_statuses()!r}, prompts {app.driver.count(PROMPT)}, "
            f"error {app.error_text()!r}"
        ),
    )


# ── the feed ─────────────────────────────────────────────────────────────────


def _archive_comments_on(app, index: int) -> list[str]:
    scope = f"post-card[{index}]"
    if not app.driver.count("post-archive-comments", scope=scope):
        return []
    return app.driver.get_texts("post-archive-comment-item", scope=scope)


def _reload_feed_until(app, post_id: str, predicate, *, poke=None, what: str):
    """Leave the feed and come back (`return_to_feed_page`: a page ENTRY, the
    reload it starts committed) until ``predicate(row, index)`` holds for the
    post — a merged reply or reaction is another person's post, which no tap
    on this seat re-reads."""
    last = {}

    def check():
        if poke is not None:
            poke_receive_cycle(poke)
        app.feed.return_to_feed_page()
        row = app.feed.post_state_by_id(post_id)
        index = app.feed.post_index_by_id(post_id)
        last.update(row=row, index=index)
        if row is None or index < 0:
            return None
        return predicate(row, index) or None

    return wait_until(
        check,
        budgets.ORCHESTRATION_STEP_S,
        interval=2,
        diagnose=lambda: (
            f"{what}; the post's state row {last.get('row')!r}, its pills "
            f"{app.feed.reaction_pills(last['index']) if last.get('index', -1) >= 0 else None!r}, "
            f"error {app.error_text()!r}"
        ),
    )


# ── the nest's post read (an assertion read, convention 5) ───────────────────


def _digest(link) -> str:
    """The 32-byte digest of a tag-42 CID link, as the hex ``post_id``."""
    value = link.value if isinstance(link, cbor2.CBORTag) else link
    return bytes(value)[-32:].hex()


def _references_to(node_url: str, author: dict, target_post_id: str) -> list[dict]:
    """Every post ``author`` wrote that references ``target_post_id``: one
    ``{"kind", "emoji", "body", "created_at", "author"}`` per post, read from
    the author's own corpus (`fauna.posts.list`) and each post's signed bytes
    (`fauna.posts.get`)."""
    items = _call(node_url, author, "fauna.posts.list", {"limit": 200}).get("posts", [])
    out = []
    for item in items:
        got = _call(node_url, author, "fauna.posts.get", {"post_id": item["post_id"]})
        decoded = cbor2.loads(bytes(got["body"]))
        post = cbor2.loads(decoded["bytes"]) if isinstance(decoded, dict) and "bytes" in decoded else decoded
        for ref in post.get("references") or []:
            ((kind, inner),) = ref.items()
            if _digest(inner["post_id"]) == target_post_id:
                out.append({
                    "kind": kind,
                    "emoji": inner.get("emoji"),
                    "body": item.get("body", ""),
                    "created_at": item["created_at"],
                    "author": bytes(post["author"]).hex(),
                })
    return out


def _replies(refs: list[dict]) -> list[str]:
    return sorted(r["body"] for r in refs if r["kind"] == "Reply")


ARCHIVE_RAIL = "Archive"  # fauna_conversations::Rail::Archive, as the state row spells it


def _thread_ids(app) -> set[str]:
    """Every listed conversation but the archive's own read-only threads —
    those come from the import's message threads (the projection lists them
    on its own schedule), never from the link exchange."""
    return {t.thread_id for t in app.conversations.list_threads() if t.rail != ARCHIVE_RAIL}


@pytest.mark.feature("import-your-social-archive")
def test_two_contacts_link_their_archives_and_each_comment_and_reaction_comes_back_signed_by_its_author(
    real_faunamls_app, second_real_faunamls_app, nest_instance, test_user, tmp_path,
):
    """Outcomes 18, 19 and 50–55 of ``docs/features/import-your-social-archive.md``
    on one journey — the seats are shared state, and each outcome is a step of
    the one before (see the module docstring for the arrangements)."""
    a_app = real_faunamls_app
    b_app, bob = second_real_faunamls_app
    node_url = nest_instance["url"]
    a_hex, b_hex = test_user["actor_id_hex"], bob["actor_id_hex"]

    suffix = uuid.uuid4().hex[:8]
    a_name, b_name = f"Ada Archive {suffix}", f"Ben Archive {suffix}"
    link = f"https://example.invalid/post/{suffix}"
    a_tag, b_tag = " " + _unique("#linka"), " " + _unique("#linkb")
    a_archive = FakeFacebookArchive(
        owner=a_name, friend_two=b_name, tag=a_tag, link=link,
        posts=[*DEFAULT_POSTS, dataclasses.replace(LINK_POST, link=link)],
    )

    def b_archive(**more) -> FakeFacebookArchive:
        return FakeFacebookArchive(owner=b_name, activity_on=a_name, tag=b_tag, link=link,
                                   love=True, friend_comment=False, **more)

    with _client(node_url, test_user) as c:
        prior_mode = c.call("fauna.inbox.mode.get", {})["mode"]
        if prior_mode != "allow_knock":
            c.call("fauna.inbox.mode.set", {"mode": "allow_knock"})
    try:
        # ── both import, before A's edge toward B exists ─────────────────────
        _import(a_app, a_archive.write(tmp_path / "a.zip"), a_name)
        a_app.driver.navigate_to("feed")
        row = a_app.feed.wait_for_post_state_by_text(LINK_TEXT + a_tag)
        assert row is not None and row.get("post_id"), f"error-message={a_app.error_text()!r}"
        post_id = row["post_id"]
        folded = wait_until(
            lambda: _archive_comments_on(a_app, a_app.feed.post_index_by_id(post_id)) or None,
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"A's private copy of B's comment never folded under the link post; "
            f"error {a_app.error_text()!r}",
        )
        assert folded == [f"{b_name}: {FRIEND_TWO_COMMENT}"], folded

        _import(b_app, b_archive().write(tmp_path / "b-1.zip"), b_name)

        # Outcome 50: A is asked about nobody. B's announce went to A, who has
        # no edge toward B — and A's roster does not even hold B's row. The
        # read waits for a receive cycle of A's that began after B's import
        # finished, so A has provably looked for whatever B sent.
        baseline = conv_receive_cycles(a_app.driver)
        poke_receive_cycle(a_app.driver)
        await_receive_cycle_after(a_app.driver, baseline[0] if baseline else None,
                                  budget_s=budgets.RECEIVE_CYCLE_S, what="B's first announce")
        open_people(a_app)
        assert a_app.driver.count(PROMPT) == 0, (
            f"A was asked before B was A's contact; rows {a_app.contacts.contact_names()!r}"
        )

        # ── the contact, through the UI: B asks, A accepts ───────────────────
        a_threads_before, b_threads_before = _thread_ids(a_app), _thread_ids(b_app)
        b_app.contacts.navigate()
        b_app.driver.clear_and_type("contact-actor-id-field", a_hex)
        b_app.driver.click("contact-actor-id-lookup")
        wait_until(
            lambda: b_app.contacts.actor_id_result_text() or None,
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"B's lookup of A found nothing; find error "
            f"{b_app.contacts.find_error_text()!r}, error {b_app.error_text()!r}",
        )
        b_app.contacts.add_contact_no_settle()
        open_people(a_app)
        # Once B's knock lands, A's roster holds B's pending row by handle, so
        # `knock-sender` reads that handle rather than the short id
        # (`value-formatting.md` § Peer display label, the roster handle).
        a_app.contacts.accept_knock(knock_row(a_app, b_hex, bob["handle"]))
        _peer_row(a_app, b_hex)
        assert wait_until(
            lambda: "accepted" in a_app.contacts.contact_status(0).lower() or None,
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"A's row for B never read accepted: {a_app.contacts.contact_statuses()!r}",
        )
        assert not _row_has(a_app, PROMPT), "accepting alone asks nothing: no import has announced since"

        # B's next finished import announces again — now to an accepted contact.
        _import(b_app, b_archive().write(tmp_path / "b-2.zip"), b_name)
        _peer_row(a_app, b_hex)
        _wait_row(a_app, PROMPT, what="A is asked once B is a contact and has announced")
        assert b_name in a_app.driver.get_text(PROMPT, scope=ROW), a_app.driver.get_text(PROMPT, scope=ROW)

        # ── outcome 51: each answers yes, once ───────────────────────────────
        a_app.driver.click(YES, scope=ROW)
        _wait_row(a_app, PROMPT, present=False, what="A's answer clears A's prompt")
        _peer_row(b_app, a_hex)
        _wait_row(b_app, PROMPT, poke=b_app.driver, what="B is asked the mirror question after A's yes")
        assert a_name in b_app.driver.get_text(PROMPT, scope=ROW), b_app.driver.get_text(PROMPT, scope=ROW)
        b_app.driver.click(YES, scope=ROW)
        _wait_row(b_app, LINKED, poke=b_app.driver, what="B's row reads linked after both answers")
        _wait_row(a_app, LINKED, what="A's row reads linked once B's confirm arrives")
        assert not _row_has(a_app, PROMPT) and not _row_has(b_app, PROMPT)
        # Asked once: re-entering Contacts never brings a prompt back.
        for app, peer in ((a_app, b_hex), (b_app, a_hex)):
            app.driver.navigate_to("feed")
            _peer_row(app, peer)
            assert _row_has(app, LINKED) and not _row_has(app, PROMPT), (
                f"after re-entry: linked={_row_has(app, LINKED)}, prompt={_row_has(app, PROMPT)}"
            )

        # ── outcomes 18 + 53: B's comment is B's own reply; A's copy is gone ─
        a_app.driver.navigate_to("feed")
        _reload_feed_until(
            a_app, post_id,
            lambda r, i: r.get("reply_count") == 1 and _archive_comments_on(a_app, i) == [],
            poke=b_app.driver,
            what="B's comment never came back as one reply with A's private copy hidden",
        )
        refs = _references_to(node_url, bob, post_id)
        replies = [r for r in refs if r["kind"] == "Reply"]
        assert [r["body"] for r in replies] == [FRIEND_TWO_COMMENT], refs
        assert replies[0]["author"] == b_hex, "the reply is signed by the person who wrote it"
        assert replies[0]["created_at"] == FRIEND_TWO_COMMENT_TS * MICROS_PER_SECOND, (
            "the reply keeps the comment's own date", replies[0])

        # ── outcome 54: the like as a like, the love as ❤️ ───────────────────
        assert [r["kind"] for r in refs if r["kind"] in ("Upvote", "React")].count("Upvote") == 1, refs
        assert [r["emoji"] for r in refs if r["kind"] == "React"] == [LOVE], refs
        counts = _call(node_url, test_user, "fauna.posts.get", {"post_id": post_id}).get("counts") or {}
        assert counts.get("like_count") == 1, counts
        assert [(g["emoji"], g["count"]) for g in counts.get("reactions", [])] == [(LOVE, 1)], counts
        # A's seat: B's like moves the like count, the love paints one pill —
        # not A's own reaction.
        _reload_feed_until(
            a_app, post_id,
            lambda r, i: r.get("like_count") == 1
            and a_app.feed.reaction_pills(i) == [{"text": f"{LOVE} 1", "state": "off"}],
            what="A's card never painted B's like as a like and B's love as one ❤️ pill",
        )

        # ── outcome 52: none of it was a message ─────────────────────────────
        assert _thread_ids(a_app) == a_threads_before, (
            "the exchange listed a thread on A's conversations page",
            [(t.label, t.snippet) for t in a_app.conversations.list_threads()])
        assert _thread_ids(b_app) == b_threads_before, (
            "the exchange listed a thread on B's conversations page",
            [(t.label, t.snippet) for t in b_app.conversations.list_threads()])

        # ── outcome 55: a newer import brings the new comment, nothing twice ─
        b3_done = _import(b_app, b_archive(second_comment=True).write(tmp_path / "b-3.zip"), b_name)
        done_at = time.monotonic()
        # Outcome 58's link half: once links exist, the Done step adds how many
        # of B's comments and reactions came back on A's posts and how many no
        # post matched (ruling 11) — the first comment, the like and the love
        # came back before this import finished, so the claimed count is read
        # from B's ledger at this run's completion, never zero.
        links_line = re.search(
            r"· (\d+) comments and reactions came back from linked contacts · (\d+) could not be matched",
            b3_done,
        )
        assert links_line and int(links_line.group(1)) >= 1, (
            f"B's Done step after linking must say what came back from linked contacts: {b3_done!r}"
        )
        a_app.driver.navigate_to("feed")
        _reload_feed_until(
            a_app, post_id, lambda r, i: r.get("reply_count") == 2,
            poke=b_app.driver, what="B's newer comment never came back",
        )
        claim_latency = time.monotonic() - done_at
        refs = _references_to(node_url, bob, post_id)
        assert _replies(refs) == sorted([FRIEND_TWO_COMMENT, SECOND_COMMENT]), refs
        second = next(r for r in refs if r["body"] == SECOND_COMMENT)
        assert second["created_at"] == SECOND_COMMENT_TS * MICROS_PER_SECOND, second
        assert sum(r["kind"] == "Upvote" for r in refs) == 1, ("the like came back twice", refs)
        assert sum(r["kind"] == "React" for r in refs) == 1, ("the love came back twice", refs)

        # ── B sees A's post with his own ❤️ marked as his ────────────────────
        sub = ApiActor(node_url, bob["token"], b_hex, bytes(bob["signing_key"]))
        reply = sub.subscribe(test_user["actor_id_bytes"], FOLLOWERS_TIER)
        assert reply.get("outcome") in ("queued", "approved", "active"), reply
        wait_until(
            lambda: sub.subscription_status(test_user["actor_id_bytes"]).get("tier") == FOLLOWERS_TIER,
            budgets.ORCHESTRATION_STEP_S, interval=2,
            diagnose=lambda: f"A's app never granted B's follow: "
            f"{sub.subscription_status(test_user['actor_id_bytes'])!r}",
        )
        b_app.driver.navigate_to("feed")
        _reload_feed_until(
            b_app, post_id,
            lambda r, i: b_app.feed.reaction_pills(i) == [{"text": f"{LOVE} 1", "state": "on"}],
            what="B's seat never painted his own ❤️ on A's post as his",
        )

        # ── outcome 19: A unlinks; B's next import adds nothing new ──────────
        _peer_row(a_app, b_hex)
        a_app.driver.click(UNLINK, scope=ROW)
        _wait_row(a_app, LINKED, present=False, what="A's unlink clears A's linked line")
        _peer_row(b_app, a_hex)
        _wait_row(b_app, LINKED, present=False, poke=b_app.driver, what="A's unlink reaches B")
        _import(b_app, b_archive(second_comment=True, third_comment=True).write(tmp_path / "b-4.zip"), b_name)
        window = max(ABSENCE_FLOOR_S, 3 * claim_latency)
        deadline = time.monotonic() + window

        def held_absent():
            poke_receive_cycle(b_app.driver)
            now = _references_to(node_url, bob, post_id)
            assert THIRD_COMMENT not in _replies(now), (
                f"an import after the unlink merged a comment ({window:.0f}s window)", now)
            return now if time.monotonic() >= deadline else None

        refs = wait_until(held_absent, window + budgets.RPC_ROUNDTRIP_S, interval=2)
        assert _replies(refs) == sorted([FRIEND_TWO_COMMENT, SECOND_COMMENT]), (
            "what already came back stays with the person who wrote it", refs)
        a_app.driver.navigate_to("feed")
        _reload_feed_until(
            a_app, post_id, lambda r, i: r.get("reply_count") == 2,
            what="A's post should still carry the two claimed replies after the unlink",
        )

        # ── 54, B's half: B taps his own ❤️ off, and both seats lose it ──────
        b_app.driver.navigate_to("feed")
        index = _reload_feed_until(
            b_app, post_id,
            lambda r, i: (i,) if b_app.feed.reaction_pills(i) else None,
            what="B's card lost its ❤️ before B tapped it",
        )[0]
        b_app.feed.toggle_reaction_pill(index, 0)
        for app in (b_app, a_app):
            app.driver.navigate_to("feed")
            _reload_feed_until(
                app, post_id, lambda r, i, app=app: app.feed.reaction_pills(i) == [],
                what="B's ❤️ was taken back but a pill still paints",
            )
    finally:
        if prior_mode != "allow_knock":
            with _client(node_url, test_user) as c:
                c.call("fauna.inbox.mode.set", {"mode": prior_mode})
