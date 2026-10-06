"""E2E coverage for the user-facing mail-lists + mail-list-members pages.

Target state: docs/goal/behavior/mail-mass-mailing.md § mail-lists page UX /
§ mail-list-members page; UX/IDs: tests/e2e-unified/ui.yaml `mail-lists` +
`mail-list-members` pages + their `*-list` components.

tier_3: a real client driver against a real fauna-nest (`logged_in_app` spins
one). The list backend is fully built — the nine `fauna.bridges.*_list_*` RPCs
are live (`bins/fauna-nest/src/bridge_list_handlers.rs`) and exercised at the
WS-RPC layer by `tests/api/test_mail_lists_send.py` — so these drive both pages
end to end (UI -> shared machine -> nest -> re-render) and assert ground truth
over the same user WS-RPC surface.

**This file was skipped from its creation until 2026-07-29, and its stated
reason had been false for six weeks.** It read "backend unbuilt: ... have no
nest handler", which stopped being true when the backend landed on 2026-06-13/14.
What was actually missing was the *client* half: `rpc_glue`'s two seams still
returned `unimplemented` for every method, so the pages rendered on five apps and
could do nothing. Rewiring the seam is what let this suite exist.

Scoped to **tui + linux + android** so far. linux's own gap — its
`mail-lists-list-item-members-button` carried no click handler ("inert in the
embedded settings seed") and its members page was wired to an all-zero
`PLACEHOLDER_LIST_ID_HEX`, so `test_mail_lists_open_members_scopes_the_page`
would have failed there for a real product reason — is fixed (2026-07-30):
`mail_lists.rs`'s Members button now calls an `on_navigate_to_members`
callback that re-targets the already-built `mail_list_members` page at the
clicked row's list (mirroring tui's `Action::MailListsOpenMembers`) and
switches the settings stack to it.

**android added 2026-07-30 as a code-reviewed marker, not
a live run** — `--client android` is host-emulator-gated fleet-wide (no
android e2e test has ever run against a real device), so this is a static
verification, not the "run --app android, fix what it finds" the other legs
did. `MailListsScreen.kt`/`MailListMembersScreen.kt` were read against all
three of linux's found bugs and against ui.yaml's element spec: the members
button already carries a real `onViewMembers` navigation callback (never
inert), `mail-lists-list-item-name` already renders `"$friendlyName ·
$address"` (friendly name + posting address, matching ui.yaml, not a
friendly-name-only render), and the domain picker cannot go stale the way
linux's did — android's `MailListsScreen`/`MailListMembersScreen` are plain
`NavHost` `composable()` destinations, so `hiltViewModel()` constructs a fresh
`MailListsVM`/`MailListMembersVM` (and re-`hydrate()`s) on every visit, unlike
linux's persisted settings-shell page. The generic bridge action layer
(`actions/mail_lists.py`'s `driver.set_state`/`click`/`get_text` calls) needs
no android-specific wiring — same shape every other `.android`-marked test
already exercises. windows/apple/web remain, not yet reviewed.

**The marker-vs-run gap above is now enforced in code, not just prose**
: `_android_leg_is_a_static_review_not_a_run` below
routes every android run of this file through `skip_unbuilt`, so it is tallied
as declared debt (and fails under `--strict-app`) rather than silently
"passing" the first time anyone actually runs `--app android` against a real
device. Delete that fixture once this file has a real device pass to show for
it.

Seeding note (the same shape as test_mail_aliases.py): the add-sheet's Create
needs a domain to create on, and the shared seam derives the picker's options
from rows the caller already owns (`derive_list_domains` — lists are user-tier,
so it must not read the Admin-class `list_local_domains`). The `logged_in_app`
actor is registered handle-less via the admin API, so no canonical alias exists;
the tests seed one exact alias over the user WS-RPC client first, then drive
the UI.

Member addresses deliberately use an **external** domain: `add_list_member`
rejects an address on a hosted local domain (`recipient_on_local_domain` — "add
an alias, not a list member").
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.app_surface import skip_unbuilt

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.android,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
]

DOMAIN = "lists-ux-e2e.test"
MEMBER_DOMAIN = "external-reader.test"


@pytest.fixture(autouse=True)
def _android_leg_is_a_static_review_not_a_run(logged_in_app):
    """A change added `pytest.mark.android` above on the strength of a code
    review alone — no Robolectric or device execution has ever backed it
    (`--client android` is host-emulator-gated fleet-wide). A marker is a
    coverage claim tooling reads at collection time; a docstring is not
    (testing.md § convention 7). Route the gap through
    the declared-debt tally instead of leaving the marker alone to claim
    execution: this self-clears the day someone runs this file `--app android`
    against a real device and deletes the call below."""
    if logged_in_app.driver.is_android():
        skip_unbuilt(
            logged_in_app.driver,
            surface="mail-lists / mail-list-members real-device e2e",
            detail="132525318a's android leg was a code review only, not a "
            "live run — no Robolectric or device execution backs it",
            tracked="",
        )


def _user_client(nest_instance, user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )


def _seed_exact(client, pattern):
    """Seed an exact alias so the add-sheet's domain picker has an option.

    The shared `derive_list_domains` sources the picker from the caller's own
    list + alias rows, so without this the picker is empty and the add button is
    correctly disabled.
    """
    client.call(
        "fauna.bridges.create_account_alias",
        {"kind": "exact", "local_domain": DOMAIN, "pattern": pattern, "controls": {"label": ""}},
    )


def _lists(client):
    return client.call("fauna.bridges.list_account_lists", {})["lists"]


def _members(client, list_id):
    return client.call(
        "fauna.bridges.list_list_members",
        {"list_id": list_id, "include_unsubscribed": True},
    )


def _create_list(client, *, friendly_name, local_part):
    return client.call(
        "fauna.bridges.create_account_list",
        {
            "local_part": local_part,
            "local_domain": DOMAIN,
            "friendly_name": friendly_name,
        },
    )["list_id"]


def _reset(client):
    """Leave the session-scoped actor as found (net-zero on the shared nest).

    Deleting a list cascades its members, so this is the whole cleanup.
    """
    for row in _lists(client):
        try:
            client.call("fauna.bridges.delete_account_list", {"list_id": row["list_id"]})
        except Exception:
            pass
    for row in client.call("fauna.bridges.list_account_aliases", {})["aliases"]:
        try:
            client.call("fauna.bridges.delete_account_alias", {"alias_id": row["alias_id"]})
        except Exception:
            pass


def _diagnose(app, api=None, list_id=None) -> str:
    """Row 29: the batch-import RPC disconnect
    investigation needs the app's own log AND a direct nest-side read alongside
    the UI-level assertion to tell "the RPC never landed" apart from "it landed
    and the render/refresh silently dropped it" (conventions point 6 /
    test_sync_live_apply.py's `_diagnose` is the established shape this
    mirrors). Passing `api`/`list_id` adds a direct `list_list_members` read —
    ground truth for whether the nest actually has the members, independent of
    anything the app rendered."""
    bits = []
    reader = getattr(app.driver, "app_log_text", None)
    if reader is not None:
        try:
            text = reader() or ""
            if not text:
                bits.append("app log: <empty — reader returned no text at all>")
            else:
                wanted = (
                    "connect", "reconnect", "disconnect", "Reconnect", "rpc",
                    "Rpc", "RPC", "batch_import", "BatchImport", "MailList",
                    "mail_list", "Warn", "Error",
                )
                lines = [ln for ln in text.splitlines() if any(w in ln for w in wanted)]
                bits.append(
                    f"app log: {len(text.splitlines())} total lines; filtered: "
                    + (" | ".join(lines[-40:]) or "<no matching lines>")
                )
        except Exception as e:  # pragma: no cover - diagnostics must never mask a failure
            bits.append(f"app log: <read failed: {e!r}>")
    else:
        bits.append("app log: <driver has no app_log_text()>")
    if api is not None and list_id is not None:
        try:
            reply = _members(api, list_id)
            bits.append(
                "nest ground truth: "
                f"{[m['recipient_address'] for m in reply['members']]!r} "
                f"(subscribed={reply['subscribed_count']})"
            )
        except Exception as e:  # pragma: no cover - diagnostics must never mask a failure
            bits.append(f"nest ground truth: <read failed: {e!r}>")
    return "; ".join(bits)


# ── mail-lists ──────────────────────────────────────────────────────


@pytest.mark.feature("mailing-lists")
def test_mail_lists_page_reachable(logged_in_app):
    """Navigate to the mail-lists page and confirm the add-button shows."""
    logged_in_app.mail_lists.navigate()
    assert logged_in_app.mail_lists.is_page_visible(), (
        "mail-lists-add-button should be visible after navigating to the "
        f"mail-lists page; error: {logged_in_app.error_text()!r}"
    )


@pytest.mark.feature("mailing-lists")
def test_mail_list_members_page_reachable(logged_in_app):
    """The members page has its own rail slot (settings.md § Navigation model),
    so it must render with nothing selected — a user with no lists still lands
    on an honest empty page rather than an error."""
    logged_in_app.mail_list_members.navigate()
    assert logged_in_app.mail_list_members.is_page_visible(), (
        "mail-list-members-add-button should be visible after navigating to the "
        f"mail-list-members page; error: {logged_in_app.error_text()!r}"
    )


@pytest.mark.feature("mailing-lists")
def test_mail_lists_add_via_sheet_lands_on_the_nest(logged_in_app, test_user, nest_instance):
    """Create a list through the add-sheet; it renders AND exists on the nest.

    The ground-truth half is what makes this more than a render test: the row
    could paint from a local optimistic guess, but `list_account_lists` answering
    with it proves the whole chain (sheet -> MailListsAction::Create -> the
    rewired seam -> `fauna.bridges.create_account_list` -> the refresh).
    """
    handle = "bob" + secrets.token_hex(3)
    local_part = "weekly" + secrets.token_hex(3)
    with _user_client(nest_instance, test_user) as api:
        _reset(api)
        _seed_exact(api, handle)
        try:
            app = logged_in_app
            app.mail_lists.navigate()
            assert app.mail_lists.is_page_visible()

            app.mail_lists.add_list("Bob's Weekly", local_part)

            names = _wait_for_row(app, 1)
            assert any("Bob's Weekly" in n for n in names), (
                f"the created list should render; rows={names!r}; "
                f"error: {app.mail_lists.error_text()!r}"
            )
            assert any(f"{local_part}@{DOMAIN}" in n for n in names), (
                f"the row must show the posting address; rows={names!r}"
            )

            # Ground truth over the same user WS-RPC surface.
            rows = _lists(api)
            assert len(rows) == 1, f"nest should hold exactly the new list, got {rows!r}"
            assert rows[0]["pattern"] == local_part
            assert rows[0]["friendly_name"] == "Bob's Weekly"
        finally:
            _reset(api)


@pytest.mark.feature("mailing-lists")
def test_mail_lists_delete_cascades_on_the_nest(logged_in_app, test_user, nest_instance):
    """The two-click inline delete removes the list from the nest, members and all."""
    handle = "carol" + secrets.token_hex(3)
    with _user_client(nest_instance, test_user) as api:
        _reset(api)
        _seed_exact(api, handle)
        list_id = _create_list(api, friendly_name="Doomed", local_part="doomed" + secrets.token_hex(3))
        api.call(
            "fauna.bridges.add_list_member",
            {"list_id": list_id, "recipient_address": f"reader@{MEMBER_DOMAIN}"},
        )
        try:
            app = logged_in_app
            app.mail_lists.navigate()
            assert app.mail_lists.is_page_visible()
            _wait_for_row(app, 1)

            app.mail_lists.delete_list(0)

            assert _wait_for_count(app, 0), (
                f"the deleted list should stop rendering; rows={app.mail_lists.names()!r}; "
                f"error: {app.mail_lists.error_text()!r}"
            )
            assert _lists(api) == [], "the nest must no longer hold the list"
        finally:
            _reset(api)


@pytest.mark.feature("mailing-lists")
def test_mail_lists_open_members_scopes_the_page(logged_in_app, test_user, nest_instance):
    """The View-members button opens the members page scoped to THAT list.

    This is the routing linux was missing until 2026-07-30 (its button carried
    no click handler and its page was wired to a placeholder list id) — the
    assertion that actually exercises the per-row scoping, not just that the
    page renders.
    """
    handle = "dave" + secrets.token_hex(3)
    with _user_client(nest_instance, test_user) as api:
        _reset(api)
        _seed_exact(api, handle)
        list_id = _create_list(
            api, friendly_name="Scoped", local_part="scoped" + secrets.token_hex(3)
        )
        api.call(
            "fauna.bridges.add_list_member",
            {"list_id": list_id, "recipient_address": f"scoped-reader@{MEMBER_DOMAIN}"},
        )
        try:
            app = logged_in_app
            app.mail_lists.navigate()
            assert app.mail_lists.is_page_visible()
            _wait_for_row(app, 1)

            app.mail_lists.open_members(0)

            assert app.mail_list_members.is_page_visible(), (
                "the View-members button must open the members page; "
                f"error: {app.error_text()!r}"
            )
            addresses = _wait_for_members(app, 1)
            assert addresses == [f"scoped-reader@{MEMBER_DOMAIN}"], (
                "the members page must be scoped to the list whose button was "
                f"clicked, got {addresses!r}"
            )
        finally:
            _reset(api)


@pytest.mark.feature("mailing-lists")
def test_mail_list_members_nothing_selected_falls_back_to_first_list(
    logged_in_app, test_user, nest_instance
):
    """`mail-list-members` has its own settings-rail slot (settings.md §
    Navigation model), reachable directly, not only via a list row's Members
    button. A visit with nothing selected must fall back to the caller's first
    owned list — tui's shape (mail-mass-mailing.md § Per-app render status) —
    rather than the honest-but-wrong empty state a raw placeholder id paints.

    This is the gap `test_mail_lists_open_members_scopes_the_page` above
    cannot exercise: that test always clicks a row's Members button first, so
    `PendingListIdHex` is never unset when the page builds its machine.
    """
    handle = "erin" + secrets.token_hex(3)
    with _user_client(nest_instance, test_user) as api:
        _reset(api)
        _seed_exact(api, handle)
        list_id = _create_list(
            api, friendly_name="Only List", local_part="only" + secrets.token_hex(3)
        )
        api.call(
            "fauna.bridges.add_list_member",
            {"list_id": list_id, "recipient_address": f"fallback-reader@{MEMBER_DOMAIN}"},
        )
        try:
            app = logged_in_app
            # Direct rail navigation — no mail_lists visit, no row click.
            app.mail_list_members.navigate()

            assert app.mail_list_members.is_page_visible(), (
                "mail-list-members-add-button should be visible after a direct "
                f"rail visit; error: {app.error_text()!r}"
            )
            addresses = _wait_for_members(app, 1)
            assert addresses == [f"fallback-reader@{MEMBER_DOMAIN}"], (
                "a direct rail visit with nothing selected must fall back to the "
                f"caller's first (here: only) owned list, got {addresses!r}"
            )
        finally:
            _reset(api)


# ── mail-list-members ───────────────────────────────────────────────


@pytest.mark.feature("mailing-lists")
def test_mail_list_members_add_and_unsubscribe_round_trip(
    logged_in_app, test_user, nest_instance
):
    """Add a member from the UI, then unsubscribe them — both land on the nest.

    Unsubscribe is **sticky** (`mail-mass-mailing.md` § Architectural rules): the
    member stays on the list with `unsubscribed_at` set, rather than being
    removed. Asserting the row survives with a flipped status is what separates a
    real unsubscribe from a delete that happens to look the same on screen.
    """
    handle = "erin" + secrets.token_hex(3)
    address = f"reader-{secrets.token_hex(3)}@{MEMBER_DOMAIN}"
    with _user_client(nest_instance, test_user) as api:
        _reset(api)
        _seed_exact(api, handle)
        list_id = _create_list(
            api, friendly_name="Round trip", local_part="rt" + secrets.token_hex(3)
        )
        try:
            app = logged_in_app
            app.mail_lists.navigate()
            assert app.mail_lists.is_page_visible()
            _wait_for_row(app, 1)
            app.mail_lists.open_members(0)
            assert app.mail_list_members.is_page_visible()

            app.mail_list_members.add_member(address)
            assert _wait_for_members(app, 1) == [address], (
                f"the added member should render; error: {app.error_text()!r}"
            )
            reply = _members(api, list_id)
            assert reply["subscribed_count"] == 1
            assert reply["members"][0]["recipient_address"] == address
            assert reply["members"][0].get("unsubscribed_at") is None

            app.mail_list_members.unsubscribe(0)

            assert _wait_for_summary(app, subscribed=0, unsubscribed=1), (
                "the summary should show the member as unsubscribed; "
                f"summary={app.mail_list_members.summary()!r}; "
                f"error: {app.error_text()!r}"
            )
            # Sticky: still a member, now flagged — not deleted.
            reply = _members(api, list_id)
            assert reply["subscribed_count"] == 0
            assert reply["unsubscribed_count"] == 1
            assert len(reply["members"]) == 1
            assert reply["members"][0].get("unsubscribed_at") is not None
        finally:
            _reset(api)


@pytest.mark.feature("mailing-lists")
def test_mail_list_members_batch_import_lands_every_valid_address(
    logged_in_app, test_user, nest_instance
):
    """Paste-import several addresses at once; the valid ones land on the nest.

    The pasted block deliberately mixes in a blank line and a syntactically
    invalid address: the nest counts those as skipped rather than failing the
    whole batch (`mail-mass-mailing.md` § mail-list-members page), so the
    assertion is that the three good addresses arrive, not that the call throws.
    """
    handle = "frank" + secrets.token_hex(3)
    tag = secrets.token_hex(3)
    good = [f"a-{tag}@{MEMBER_DOMAIN}", f"b-{tag}@{MEMBER_DOMAIN}", f"c-{tag}@{MEMBER_DOMAIN}"]
    pasted = f"{good[0]}\n\n{good[1]}\n   \nnot-an-address\n{good[2]}"
    with _user_client(nest_instance, test_user) as api:
        _reset(api)
        _seed_exact(api, handle)
        list_id = _create_list(
            api, friendly_name="Import", local_part="imp" + secrets.token_hex(3)
        )
        try:
            app = logged_in_app
            app.mail_lists.navigate()
            assert app.mail_lists.is_page_visible()
            _wait_for_row(app, 1)
            app.mail_lists.open_members(0)
            assert app.mail_list_members.is_page_visible()

            app.mail_list_members.batch_import(pasted)

            assert _wait_for_members(app, 3), (
                "all three valid addresses should render; "
                f"rows={app.mail_list_members.addresses()!r}; "
                f"error: {app.error_text()!r}; "
                f"{_diagnose(app, api, list_id)}"
            )
            reply = _members(api, list_id)
            assert sorted(m["recipient_address"] for m in reply["members"]) == sorted(good)
            assert reply["subscribed_count"] == 3
        finally:
            _reset(api)


# ── waiters ─────────────────────────────────────────────────────────
#
# Latency-independent (testing.md point 14): each polls the rendered state to a
# named generous ceiling and returns the instant it is satisfied, so a green run
# pays nothing and a loaded box does not turn a correct page into a red test.
# None of them asserts on wall-clock timing.

_RENDER_BUDGET_S = 30.0


def _deadline_poll(predicate, budget=_RENDER_BUDGET_S):
    import time

    end = time.monotonic() + budget
    while True:
        result = predicate()
        if result:
            return result
        if time.monotonic() >= end:
            return result
        time.sleep(0.1)


def _wait_for_row(app, count):
    """Wait until the lists page renders `count` rows; return the names."""
    _deadline_poll(lambda: app.mail_lists.row_count() >= count)
    return app.mail_lists.names()


def _wait_for_count(app, count):
    return _deadline_poll(lambda: app.mail_lists.row_count() == count) or (
        app.mail_lists.row_count() == count
    )


def _wait_for_members(app, count):
    """Wait until the members page renders `count` rows; return the addresses."""
    _deadline_poll(lambda: app.mail_list_members.row_count() >= count)
    return app.mail_list_members.addresses()


def _wait_for_summary(app, *, subscribed, unsubscribed):
    """Wait until the summary reads the given counts (it is rendered from the
    nest's own totals, so this is the page agreeing with the backend)."""

    def ok():
        text = app.mail_list_members.summary()
        return str(subscribed) in text and str(unsubscribed) in text

    return _deadline_poll(ok) or ok()
