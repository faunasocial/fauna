"""Shared pieces of the Search-page journeys that read a result's KIND, make the
nest arm FAIL, or seed an unsent DRAFT — used by `test_search_outcomes.py` and
`test_search_private_index.py`, so the two pages' witnesses read rows, fail the
nest and clean up the same way.

**Reading a row's kind.** Every app paints the shared badge — a
`LocalizedText` from `fauna_client_search::kind::class_badge_key` — inside the
`search-result-item`, beside the cleaned snippet (`docs/goal/ui/search.md`
§ State & data shape). A row's kind is read back as a whole word of its text,
from the generated string table, so a copy edit is never a test edit.

**Failing the nest arm, for real.** The Search page fires two arms at once — the
nest's `fauna.search.query` and the local sealed index — and commits them
TOGETHER, leaving the previous page painted until then (`SearchManager::begin` /
`fetch_and_commit`). Holding the nest reply (`helpers/rpc_hold.py`, a
test-hooks surface) makes the client give up on it at the kind's own deadline,
which is a genuine nest-arm failure; while the hold is armed, EVERY row a search
commits is therefore the local index's. Two consequences the helpers encode:

* a row is only evidence once the page held nothing like it before the hold —
  callers empty the page first (`empty_the_page`);
* a re-query supersedes the one still in flight, so a held search must be given
  its own commit before the next is issued (`FailingNestArm.find`).
"""

from __future__ import annotations

import re
import uuid

from helpers.rpc_hold import arm_rpc_hold, release_rpc_hold
from helpers.waiting import wait_rail_blob_settled, wait_until
from i18n.strings import S

#: The one wire kind the Search page's nest arm issues (`search.md` § The
#: page's wire surface).
SEARCH_KIND = "fauna.search.query"

#: A nest-arm hit is one round trip after submit — a ceiling a green run never
#: pays (convention 14).
NEST_HIT_S = 30.0

#: A local-index hit waits on the builder's flush: a 5 s quiet-period debounce
#: with a 60 s periodic backstop (`libs/fauna-client-index/src/lifecycle.rs`),
#: so the ceiling clears the backstop with margin — the budget
#: `test_search_local_index.py` uses.
LOCAL_INDEX_S = 90.0

#: One held search's own commit: the client abandons the held nest arm at the
#: kind's 5 s deadline (`fauna_protocol::kind`), then commits both arms. Far
#: above any healthy run; a tick that misses it simply searches again.
HELD_SETTLE_S = 20.0

#: A seat's debounced autosave onto a reserved rail (1.5 s —
#: `fauna_client_drafts::autosave_debounce`; the MLS replica's own debounce is
#: the same order) plus one PUT and a settle: the ceiling on a rail blob the
#: phone-finds journeys gate a desktop launch on. A green run pays seconds.
RAIL_UPLOAD_S = 25.0

#: The conversations-rail key of the `__drafts` reserved folder
#: (`fauna_protocol::drafts::RAIL_CONVERSATIONS`) — where the new-thread draft
#: `start_new_thread_draft` seeds comes to rest.
DRAFTS_RAIL = "conversations"

#: Every badge label a row can carry.
BADGES = frozenset({
    S.search_page.badge_post,
    S.search_page.badge_profile,
    S.search_page.badge_email,
    S.search_page.badge_event,
    S.search_page.badge_message,
    S.search_page.badge_contact,
    S.search_page.badge_file,
    S.search_page.badge_draft,
    S.search_page.badge_media,
})


def kinds_in(text: str) -> set[str]:
    """The badge labels a row's text carries, as whole words — `File` must not
    be read out of `filed`."""
    return {b for b in BADGES if re.search(rf"\b{re.escape(b)}\b", text)}


def kinds_shown(app) -> set[str]:
    """The union of kinds across every painted row — empty when there are none,
    so "only kind X" can never be satisfied by an empty page."""
    shown: set[str] = set()
    for text in app.search.result_texts():
        shown |= kinds_in(text)
    return shown


def rows(app) -> str:
    """Every row's text, for a failure message (convention 6)."""
    return repr(app.search.result_texts())


def row_with(app, needle: str, kind: str) -> str | None:
    """The first painted row carrying `needle` whose ONLY kind is `kind`."""
    for text in app.search.result_texts():
        if needle in text and kinds_in(text) == {kind}:
            return text
    return None


def found(app, needle: str, kind: str) -> str | None:
    """Search `needle` once, both arms healthy; the row if one of `kind` shows.
    A poll tick: a local-index hit needs a re-query per tick, not just a wait on
    a page that already committed."""
    app.search.query(needle)
    return row_with(app, needle, kind)


def empty_the_page(app) -> None:
    """Commit a search that matches nothing, so a row seen afterwards can only
    come from a later search — the page keeps its previous rows until the next
    commit."""
    app.search.query(f"zzznothing{uuid.uuid4().hex[:10]}")
    wait_until(
        app.search.has_no_results,
        NEST_HIT_S,
        diagnose=lambda: f"a no-match search should empty the page; rows={rows(app)}",
    )


class FailingNestArm:
    """While entered, the nest never answers a search: every search the apps
    issue fails its nest arm at the client's own deadline, and whatever rows
    it still commits came from the local index.

    **No arrival check, on purpose.** `wait_for_held_rpc` cannot witness these
    searches: tui's test agent awaits a gesture's network half, so the submit
    click returns only after the search has COMMITTED — by which time the
    client has abandoned the held request, its `Cancel` has aborted the parked
    dispatch, and the hook's `holding` count is back to 0 with nothing
    `released` (`rpc_hold_test_hook.rs` — `HoldingGuard`). Soundness rests on
    the caller instead: the page is emptied BEFORE the hold is armed
    (`empty_the_page`), so any row for the needle afterwards is the commit of
    a search issued while every nest answer was being withheld."""

    def __init__(self, port: int):
        self.port = port

    def __enter__(self) -> "FailingNestArm":
        arm_rpc_hold(self.port, SEARCH_KIND)
        return self

    def __exit__(self, *exc) -> None:
        # Unconditional: an armed kind parks every later search on this
        # session-scoped nest, in every test that runs after this one.
        release_rpc_hold(self.port, SEARCH_KIND)

    def find(self, app, needle: str, kind: str) -> str | None:
        """One held search for `needle`; the `kind` row it commits, or None.

        Gives the search its own commit before the caller's next tick — a
        search issued while this one is still in flight would supersede it, and
        on an app whose submit returns before the commit that would starve
        every tick."""
        app.search.query(needle)
        try:
            return wait_until(lambda: row_with(app, needle, kind), HELD_SETTLE_S)
        except AssertionError:
            return None


def _open_new_thread_composer(app) -> None:
    """Land on the conversations page with the new-thread composer open — its
    `new-conversation-cancel` visible — opening it only when it is not already.

    Never `ConversationsActions.navigate()`, which waits for the list's
    `new-conversation-button`: on a phone the composer is a PUSHED screen that
    covers the list, so while a draft is open that button is not in the tree at
    all (`ConversationsListView.swift`, iOS), and the desktop's inline composer
    keeps its `+` beside it anyway. An open composer is simply reused —
    re-opening keeps the stashed draft on every app, so nothing is lost by not
    pressing `+` again."""
    app.driver.navigate_to("conversations")
    if app.driver.is_visible("new-conversation-cancel"):
        return
    app.driver.wait_for("new-conversation-button", timeout=10.0)
    app.driver.click("new-conversation-button")
    app.driver.wait_for("new-conversation-cancel", timeout=10.0)


def start_new_thread_draft(app, body: str) -> None:
    """Type `body` into the new-thread composer and leave it unsent — a draft.
    `clear_and_type`, never `type_text`: a draft another test left behind would
    otherwise be extended rather than replaced. A composer already open (the
    edit of a draft this journey wrote) is typed into as it is."""
    _open_new_thread_composer(app)
    app.driver.wait_for("dm-text-field", timeout=10.0)
    app.driver.clear_and_type("dm-text-field", body)


def discard_new_thread_draft(app) -> None:
    """The user-facing discard (`conversations.md` § Persistence), through the
    UI. `__drafts` is per-actor nest state on the session-scoped user, so a
    draft left behind is restored into the next test's composer
    (`test_conversations_draft_persistence.py` records the leak it caused).
    Re-opening the composer keeps the stashed draft, so cancel is reachable
    whether or not the composer was left open."""
    _open_new_thread_composer(app)
    app.driver.click("new-conversation-cancel")


def reset_search_page(app) -> None:
    """Leave the session-cached app's Search page as a fresh one finds it.
    `search-cancel-button` resets the page to its pre-search snapshot — the
    filter back on All (`SearchSnapshot::default`) and the error cleared. The
    manager outlives the test, so a narrowed filter or a failed-search error
    left behind would steer the next test's query."""
    app.search.navigate()
    if app.driver.is_visible("search-cancel-button"):
        app.search.cancel()


def actor_client(nest_instance, test_user):
    """A raw User-class WS-RPC connection as the session user — a read-only
    'device' observing her reserved rails (`fauna.drafts.get`, `fauna.mls.get`,
    `fauna.index.list`). Open per call: ``with actor_client(...) as c``."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=test_user["actor_id_bytes"],
        signing_key=bytes(test_user["signing_key"]),
    )


def drafts_blob(nest_instance, test_user):
    """The sealed `__drafts` conversations blob right now (`None` when never
    persisted) — read BEFORE seeding a draft, so `wait_drafts_uploaded` can
    insist on a newer one."""
    with actor_client(nest_instance, test_user) as c:
        return c.call("fauna.drafts.get", {"path": DRAFTS_RAIL}).get("blob")


def wait_drafts_uploaded(nest_instance, test_user, *, superseding):
    """The seat's autosave sealed its drafts into `__drafts`: the blob is
    present, differs from `superseding`, and has settled. Returns the settled
    blob. A desktop seat restores another seat's draft at ITS launch and only
    then (`reserved-folders.md` § Drafts Sync — load-on-launch, never
    push-on-write), so a launch that must find the draft waits on this first."""
    return wait_rail_blob_settled(
        lambda: actor_client(nest_instance, test_user),
        "fauna.drafts.get",
        DRAFTS_RAIL,
        superseding=superseding,
        timeout=RAIL_UPLOAD_S,
    )


def replica_blob(nest_instance, test_user, path):
    """The sealed `__mls` replica blob at `path` right now (`None` when absent)
    — read BEFORE the change `wait_replica_uploaded` waits for."""
    with actor_client(nest_instance, test_user) as c:
        return c.call("fauna.mls.get", {"path": path}).get("blob")


def wait_replica_uploaded(nest_instance, test_user, path, *, superseding=None):
    """The seat's MLS-sync autosave uploaded replica `path` (`provider`,
    `history/<ch>`): present, newer than `superseding`, settled. Returns the
    settled blob. What a desktop seat of the same account restores its
    conversations from at launch (`devices.md` § Cross-device MLS group-state
    sync — own plaintext rides the history slice)."""
    return wait_rail_blob_settled(
        lambda: actor_client(nest_instance, test_user),
        "fauna.mls.get",
        path,
        superseding=superseding,
        timeout=RAIL_UPLOAD_S,
    )
