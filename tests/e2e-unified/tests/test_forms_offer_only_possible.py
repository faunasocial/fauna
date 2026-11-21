"""The feed, search and nostr forms offer only a submit that can succeed: each
journey empties or malforms a draft through the page itself and reads what the
page offers then, and where a form saves, makes the save fail and reads that
the form is still open with what was typed.

Goal: ``docs/goal/ui/README.md`` § Offer only what is possible (rule 2: what
the person can unblock is disabled with its reason within eyeshot; rule 4's
form case: a form's submit is painted from its draft-validity predicate, the
submit asks the same predicate, and the form closes only once its save has
succeeded). The per-page instances: ``docs/goal/ui/feed.md`` § Errors & edge
cases (*Form validity is answered before the submit*), ``docs/goal/ui/search.md``
§ Errors & edge cases (*A blank query is not offered*) and
``docs/goal/ui/nostr.md`` § Errors & edge cases (*Form validity is answered
before the submit*).

**The witness rule.** ``docs/goal/architecture/feature-catalog.md`` § The
coverage contract: a possibility outcome is witnessed by a journey on its own
page, which fails when the control is enabled while its precondition is unmet,
when its reason is not within eyeshot, or when a control this person can never
use is shown. The shared predicates' machine tests (``libs/fauna-feed/src/forms.rs``,
``fauna_client_search::query_possibility``, ``libs/fauna-client-bridges/src/nostr_forms.rs``,
``fauna_protocol::nostr_relay::add_relay_possibility``) and tui's in-process
walk are not that witness.

**The reason is read as painted.** ``PlatformDriver.blocked_reason`` answers
what the app shows beside the control, and the journey compares it with the
``S.`` string of the predicate's reason key, never a literal. An app whose
driver cannot read it yet takes the ``blocked-reason-read`` surface gate.

**Pressing a blocked control.** On an app that paints the submit from the
predicate the disabled control IS the refusal: no person can press it, and the
test agent refuses a press on a disabled control (convention 11), so a journey
never presses one. The press half of each ruling (the submit refusing with the
same reason) is the shared predicate's backstop, pinned by tui's unit tests.

**A save that fails.** ``helpers/rpc_hold.refuse_rpc`` turns the save's own
request away at the nest without running its handler; the journey then reads
the still-open form and its typed draft, releases the refusal, presses the same
submit again and reads the form close — so each save journey also leaves the
shared session's composer, feed list and bridge feeds as it found them.

Every act the outcomes name goes through the page (convention 8); the waits
are generous deadlines over a local repaint or a named request, never settle
sleeps (convention 14).
"""

from __future__ import annotations

import uuid
from pathlib import Path

import pytest

from helpers import budgets
from helpers.app_surface import require_surface
from helpers.rpc_hold import refuse_rpc, release_rpc_hold, wait_for_refused_rpc
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

POSTS_CREATE = "fauna.posts.create"
FEED_CREATE = "fauna.feed.create"
BRIDGE_FEED_CREATE = "fauna.bridges.feeds.create"

FIXTURE_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"

# A well-formed public relay address (the same shape `test_nostr.py` adds).
RELAY_URL = "wss://relay-row-6220.example.com"


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _diag(driver, element_id: str, scope: str | None = None) -> str:
    return (
        f"{driver.diagnose(element_id, attrs=('enabled', 'reason', 'title'), scope=scope)}, "
        f"{driver.diagnose('error-message')}"
    )


def _tree_lines(driver, *ids: str) -> str:
    """The driver's registry lines naming ``ids`` (each slot's visibility state,
    frame and scope path) — what tells a closed dialog from one whose own
    registrations merely read hidden. Empty where the driver has no tree."""
    lines = [ln.strip() for ln in (driver.tree() or "").splitlines()
             if any(i in ln for i in ids)]
    return " | ".join(lines) or "<no tree>"


def _blocked_with(driver, element_id: str, reason: str, scope: str | None) -> bool:
    if driver.is_absent(element_id, scope=scope):
        return False
    if driver.is_enabled(element_id, scope=scope):
        return False
    return driver.blocked_reason(element_id, scope=scope) == reason


def _assert_blocked_with(driver, element_id: str, reason: str, *, what: str,
                         scope: str | None = None) -> None:
    """``element_id`` is shown, not live, and the words ``reason`` are painted
    beside it. The poll is a deadline over the repaint that follows the last
    keystroke; the asserts after it name every part that failed (convention 6)."""
    try:
        wait_until(lambda: _blocked_with(driver, element_id, reason, scope),
                   budgets.UI_SETTLE_S)
    except AssertionError:
        pass  # the asserts below say which half failed
    diag = _diag(driver, element_id, scope)
    assert not driver.is_absent(element_id, scope=scope), (
        f"{what}: {element_id} must stay visible while the person can unblock it "
        f"(ui/README.md § Offer only what is possible, rule 2): {diag}"
    )
    assert not driver.is_enabled(element_id, scope=scope), (
        f"{what}: {element_id} is live while its draft cannot be saved: {diag}"
    )
    painted = driver.blocked_reason(element_id, scope=scope)
    assert painted == reason, (
        f"{what}: the reason beside {element_id} should read {reason!r}, "
        f"painted {painted!r}: {diag}"
    )


def _assert_offered(driver, element_id: str, *, what: str, scope: str | None = None) -> None:
    """The draft is complete: the submit is live again (so the gate follows the
    draft, never a control that stays disabled)."""
    wait_until(
        lambda: driver.is_enabled(element_id, scope=scope),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"{what}: {element_id} stayed disabled on a complete draft: "
                         f"{_diag(driver, element_id, scope)}",
    )


def _page_error(app) -> str:
    try:
        return app.error_text() or ""
    except Exception:  # noqa: BLE001 - an absent error element reads as none
        return ""


def _post_card_index(driver, text: str) -> int:
    for i in range(driver.count("post-card")):
        if text in (driver.get_text("post-card", index=i) or ""):
            return i
    return -1


# ── feed-compose 9 ────────────────────────────────────────────────────────────


@pytest.mark.feature("feed-compose")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_post_and_reply_are_blocked_while_empty_and_a_failed_send_keeps_the_dialog_open(
    logged_in_app, nest_instance,
):
    """The compose dialog's Post and a reply dialog's Reply are disabled with
    their reasons while the text is empty; a send the nest turns away leaves
    each dialog open with the text (and the post's attachment) and the error on
    the page; the same press after the nest takes posts again sends it and
    closes the dialog."""
    app = logged_in_app
    d = app.driver
    feed = app.feed
    require_surface(d, "blocked-reason-read")
    port = nest_instance["port"]
    dialog = "feed-compose-dialog"

    feed.navigate()
    d.wait_for("feed-view")

    # 1. The compose dialog with nothing typed: Post is blocked, its reason beside it.
    feed.open_compose_dialog()
    d.wait_for(dialog, timeout=10)
    d.clear_and_type("compose-text-field", "")
    _assert_blocked_with(d, "post-submit-button", S.feed.compose_empty,
                         scope=dialog, what="an empty post")

    # 2. Text and an attachment, the send refused: the dialog stays with both.
    text = _unique("row-6220-post")
    d.clear_and_type("compose-text-field", text)
    d.set_input_files("compose-file", str(FIXTURE_IMAGE))
    wait_until(
        lambda: d.is_visible("compose-file-ready"),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"the attachment was never named: {d.diagnose('compose-file-ready')}",
    )
    _assert_offered(d, "post-submit-button", scope=dialog, what="a post with text")
    refuse_rpc(port, POSTS_CREATE)
    try:
        d.click("post-submit-button", scope=dialog)
        wait_for_refused_rpc(port, POSTS_CREATE)
        error = wait_until(
            lambda: _page_error(app),
            budgets.UI_SETTLE_S,
            diagnose=lambda: f"a refused send said nothing on error-message: {_diag(d, dialog)}",
        )
        assert d.is_visible(dialog), (
            f"a refused send closed the compose dialog (feed.md § Errors & edge cases: "
            f"it closes only once the post is saved); error={error!r}; "
            f"dialog: {d.diagnose(dialog)}; its field: "
            f"{d.diagnose('compose-text-field', scope=dialog)}; its Post: "
            f"{_diag(d, 'post-submit-button', scope=dialog)}; tree: "
            f"{_tree_lines(d, dialog, 'post-submit-button', 'error-message', 'compose-error')}"
        )
        assert feed.compose_body_text() == text, (
            f"a refused send must keep what was typed; got {feed.compose_body_text()!r}, "
            f"error={error!r}"
        )
        assert d.is_visible("compose-file-ready"), (
            f"a refused send must keep the staged attachment: "
            f"{d.diagnose('compose-file-ready')}, error={error!r}"
        )
    finally:
        release_rpc_hold(port, POSTS_CREATE)

    # 3. The same press once the nest takes posts: it lands and the dialog closes.
    d.click("post-submit-button", scope=dialog)
    assert feed.wait_for_post_text(text, timeout_s=budgets.RPC_ROUNDTRIP_S), (
        f"the retried post never landed; error={_page_error(app)!r}"
    )
    wait_until(
        lambda: d.is_absent(dialog),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"the compose dialog stayed open after its post was saved: "
                         f"{d.diagnose(dialog)}, error={_page_error(app)!r}",
    )

    # 4. A reply dialog with nothing typed: Reply is blocked, its reason beside it.
    wait_until(
        lambda: _post_card_index(d, text) >= 0,
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"the saved post is not on the feed: {d.diagnose('post-card')}",
    )
    index = _post_card_index(d, text)
    d.click("feed-reply-button", scope=f"post-card[{index}]")
    d.wait_for("feed-reply-text-field", timeout=10)
    _assert_blocked_with(d, "feed-reply-submit-button", S.feed.reply_empty,
                         what="an empty reply")

    # 5. A reply the nest turns away: the dialog stays with what was typed.
    reply = _unique("row-6220-reply")
    d.clear_and_type("feed-reply-text-field", reply)
    _assert_offered(d, "feed-reply-submit-button", what="a reply with text")
    refuse_rpc(port, POSTS_CREATE)
    try:
        d.click("feed-reply-submit-button")
        wait_for_refused_rpc(port, POSTS_CREATE)
        error = wait_until(
            lambda: _page_error(app),
            budgets.UI_SETTLE_S,
            diagnose=lambda: "a refused reply said nothing on error-message: "
                             f"{_diag(d, 'feed-reply-dialog')}",
        )
        assert d.is_visible("feed-reply-dialog"), (
            f"a refused reply closed its dialog; error={error!r}"
        )
        kept = d.get_text("feed-reply-text-field") or ""
        assert kept == reply, (
            f"a refused reply must keep what was typed; got {kept!r}, error={error!r}"
        )
    finally:
        release_rpc_hold(port, POSTS_CREATE)

    # 6. The same press once the nest takes posts: the dialog closes.
    d.click("feed-reply-submit-button")
    wait_until(
        lambda: d.is_absent("feed-reply-dialog"),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the reply dialog stayed open after its reply was saved: "
                         f"{d.diagnose('feed-reply-dialog')}, error={_page_error(app)!r}",
    )


# ── custom-feeds 13 ───────────────────────────────────────────────────────────


@pytest.mark.feature("custom-feeds")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_create_feed_is_blocked_while_unnamed_and_a_failed_save_keeps_the_page_open(
    logged_in_app, nest_instance,
):
    """Create is disabled and says a name is needed while the name is blank; a
    save the nest turns away leaves the create page open with the name; the
    same press after the nest takes it saves the feed and closes the page."""
    app = logged_in_app
    d = app.driver
    feed = app.feed
    require_surface(d, "blocked-reason-read")
    port = nest_instance["port"]

    feed.navigate()
    d.wait_for("feed-view")
    d.click("feed-create-feed-button")
    d.wait_for("feed-create-feed-name", timeout=10)
    try:
        d.clear_and_type("feed-create-feed-name", "")
        _assert_blocked_with(d, "create-feed", S.feed.create.name_required,
                             what="an unnamed feed")

        name = _unique("row-6220-feed")
        d.clear_and_type("feed-create-feed-name", name)
        _assert_offered(d, "create-feed", what="a named feed")
        refuse_rpc(port, FEED_CREATE)
        try:
            d.click("create-feed")
            wait_for_refused_rpc(port, FEED_CREATE)
            error = wait_until(
                lambda: _page_error(app),
                budgets.UI_SETTLE_S,
                diagnose=lambda: f"a refused save said nothing on error-message: "
                                 f"{_diag(d, 'create-feed')}",
            )
            assert d.is_visible("feed-create-feed-name"), (
                f"a refused save closed the create page (feed.md § Errors & edge cases: "
                f"it closes only once the feed is saved); error={error!r}"
            )
            kept = d.get_text("feed-create-feed-name") or ""
            assert kept == name, (
                f"a refused save must keep the typed name; got {kept!r}, error={error!r}"
            )
        finally:
            release_rpc_hold(port, FEED_CREATE)

        d.click("create-feed")
        wait_until(
            lambda: d.is_absent("feed-create-feed-name"),
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"the create page stayed open after the feed was saved: "
                             f"{d.diagnose('feed-create-feed-name')}, error={_page_error(app)!r}",
        )
    finally:
        feed.close_create_feed_form()


# ── bridges 16 ────────────────────────────────────────────────────────────────


@pytest.mark.feature("bridges")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_bridge_subscribe_is_blocked_until_complete_and_a_failed_subscribe_keeps_the_form(
    logged_in_app, nest_instance,
):
    """The bridge-subscribe form's Subscribe is disabled and says which field
    is missing while no bridge is picked or no address is typed; a subscribe
    the nest turns away leaves the form open with the address; the same press
    after the nest takes it subscribes and closes the form.

    A bridge must be available for the form to exist at all (it is absent
    otherwise — rule 2's never-possible case, `test_feed_bridge_selector_gating.py`);
    the account links nostr through its own page, the self-serve way
    `test_feed_bridge_subscribe.py` does, and restores the unlinked state."""
    app = logged_in_app
    d = app.driver
    require_surface(d, "blocked-reason-read")
    port = nest_instance["port"]

    app.nostr.navigate()
    assert app.nostr.is_page_visible(), f"nostr page unreachable; error={_page_error(app)!r}"
    we_linked = False
    if not app.nostr.is_linked():
        app.nostr.link_generate()
        assert app.nostr.wait_for_linked(), (
            f"generate-link should link the account; error={app.nostr.page_error_text()!r}"
        )
        we_linked = True
    try:
        app.driver.navigate_to("feed")
        d.wait_for("feed-view")
        d.wait_for("bridge-feed-subscribe-toggle", timeout=20)
        d.click("bridge-feed-subscribe-toggle")
        d.wait_for("bridge-form-uri-input", timeout=10)

        # No address typed: blocked with the reason naming what is missing — the
        # bridge first if none is picked, else the address.
        d.clear_and_type("bridge-form-uri-input", "")
        picked = (d.get_text("bridge-form-bridge-select") or "").strip()
        missing = (S.feed.bridge_form.uri_required if picked
                   else S.feed.bridge_form.kind_required)
        _assert_blocked_with(d, "bridge-form-subscribe-button", missing,
                             what=f"a subscribe with no address (bridge {picked!r})")
        d.select("bridge-form-bridge-select", "nostr")
        _assert_blocked_with(d, "bridge-form-subscribe-button",
                             S.feed.bridge_form.uri_required,
                             what="a subscribe with a bridge and no address")

        uri = f"nostr://npub1row6220{uuid.uuid4().hex[:8]}"
        d.clear_and_type("bridge-form-uri-input", uri)
        _assert_offered(d, "bridge-form-subscribe-button", what="a complete subscribe")
        refuse_rpc(port, BRIDGE_FEED_CREATE)
        try:
            d.click("bridge-form-subscribe-button")
            wait_for_refused_rpc(port, BRIDGE_FEED_CREATE)
            error = wait_until(
                lambda: _page_error(app),
                budgets.UI_SETTLE_S,
                diagnose=lambda: "a refused subscribe said nothing on error-message: "
                                 f"{_diag(d, 'bridge-form-subscribe-button')}",
            )
            assert d.is_visible("bridge-form-uri-input"), (
                f"a refused subscribe closed the form; error={error!r}"
            )
            kept = d.get_text("bridge-form-uri-input") or ""
            assert kept == uri, (
                f"a refused subscribe must keep the typed address; got {kept!r}, "
                f"error={error!r}"
            )
        finally:
            release_rpc_hold(port, BRIDGE_FEED_CREATE)

        before = d.count("bridge-feed-unsubscribe-button")
        d.click("bridge-form-subscribe-button")
        wait_until(
            lambda: d.is_absent("bridge-form-uri-input"),
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"the subscribe form stayed open after the subscribe saved: "
                             f"{d.diagnose('bridge-form-uri-input')}, error={_page_error(app)!r}",
        )
        wait_until(
            lambda: d.count("bridge-feed-unsubscribe-button") > before,
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"no new bridge feed row after the subscribe; "
                             f"error={_page_error(app)!r}",
        )
        # Leave the bridge feeds as found.
        while d.count("bridge-feed-unsubscribe-button") > before:
            n = d.count("bridge-feed-unsubscribe-button")
            d.click("bridge-feed-unsubscribe-button", index=n - 1)
            wait_until(lambda: d.count("bridge-feed-unsubscribe-button") < n,
                       budgets.RPC_ROUNDTRIP_S)
    finally:
        if we_linked:
            app.nostr.navigate()
            app.nostr.unlink()
            app.nostr.wait_for_unlinked()


# ── search 12 ─────────────────────────────────────────────────────────────────


@pytest.mark.feature("search")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_search_is_blocked_with_its_reason_while_the_field_is_blank(logged_in_app):
    """Search is disabled and says to type a query while the field is blank,
    comes alive once something is typed, and is blocked again once the field
    is emptied."""
    app = logged_in_app
    d = app.driver
    require_surface(d, "blocked-reason-read")

    app.search.navigate()
    d.wait_for("search-query-field", timeout=10)
    d.clear_and_type("search-query-field", "")
    _assert_blocked_with(d, "search-submit-button", S.search_page.query_required,
                         what="a blank search")

    d.clear_and_type("search-query-field", "row6220")
    _assert_offered(d, "search-submit-button", what="a typed search")

    d.clear_and_type("search-query-field", "   ")
    _assert_blocked_with(d, "search-submit-button", S.search_page.query_required,
                         what="a search of spaces only")


# ── nostr 32 ──────────────────────────────────────────────────────────────────


@pytest.mark.feature("nostr")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_nostr_link_follow_zap_signer_and_relay_are_blocked_with_their_reasons(logged_in_app):
    """Link (import with no nsec), add-follow (no pubkey), add-zap-signer (no
    pubkey, then a malformed one) and add-relay (no address, then one already
    listed) are each disabled with the reason beside them, and a duplicate relay
    stays in the field rather than being cleared silently. Restores the relay
    list and the link state it changed."""
    app = logged_in_app
    d = app.driver
    nostr = app.nostr
    require_surface(d, "blocked-reason-read")

    nostr.navigate()
    assert nostr.is_page_visible(), f"nostr page unreachable; error={_page_error(app)!r}"
    was_linked = nostr.is_linked()
    if was_linked:
        nostr.unlink()
        assert nostr.wait_for_unlinked(), (
            f"unlink should return to the link form; error={nostr.page_error_text()!r}"
        )
    try:
        # Link, import mode, no nsec typed.
        d.wait_for("nostr-link-mode", timeout=10)
        d.select("nostr-link-mode", "import")
        d.wait_for("nostr-nsec-input", timeout=10)
        d.clear_and_type("nostr-nsec-input", "")
        _assert_blocked_with(d, "nostr-link-button", S.nostr.link_account.enter_nsec,
                             what="an import link with no nsec")

        # Back to the generate mode, whose Link needs no field, then link.
        d.select("nostr-link-mode", "generate")
        _assert_offered(d, "nostr-link-button", what="a generate link")
        nostr.link_generate()
        assert nostr.wait_for_linked(), (
            f"generate-link should link the account; error={nostr.page_error_text()!r}"
        )

        # Add-follow, no pubkey typed.
        d.wait_for("nostr-follow-pubkey-input", timeout=10)
        d.clear_and_type("nostr-follow-pubkey-input", "")
        _assert_blocked_with(d, "nostr-add-follow", S.nostr.follows.pubkey_required,
                             what="a follow with no pubkey")

        # Add-zap-signer, no pubkey, then a malformed one.
        assert nostr.wait_for_zap_signer_section(), (
            f"the zap signer section should render for a linked account: "
            f"{d.diagnose('nostr-zap-signer-pubkey-input')}"
        )
        d.clear_and_type("nostr-zap-signer-pubkey-input", "")
        _assert_blocked_with(d, "nostr-zap-signer-add-btn",
                             S.nostr.zap_signers.pubkey_required,
                             what="a zap signer with no pubkey")
        d.clear_and_type("nostr-zap-signer-pubkey-input", "abc123")
        _assert_blocked_with(d, "nostr-zap-signer-add-btn",
                             S.nostr.zap_signers.invalid_pubkey,
                             what="a zap signer whose pubkey is not 64 hex")
        d.clear_and_type("nostr-zap-signer-pubkey-input", "")

        # Add-relay, no address, then one already listed.
        nostr.clear_relays()
        assert nostr.wait_for_relay_count(0)
        d.clear_and_type("nostr-relay-input", "")
        _assert_blocked_with(d, "nostr-add-relay", S.nostr.relays.url_required,
                             what="a relay with no address")
        nostr.add_relay(RELAY_URL)
        assert nostr.wait_for_relay_count(1), (
            f"the relay should be listed; error={nostr.page_error_text()!r}"
        )
        d.clear_and_type("nostr-relay-input", RELAY_URL)
        _assert_blocked_with(d, "nostr-add-relay", S.nostr.relays.already_added,
                             what="a relay already in the list")
        kept = d.get_text("nostr-relay-input") or ""
        assert kept == RELAY_URL, (
            f"a duplicate relay must stay in the field, never cleared silently; "
            f"got {kept!r}"
        )
        assert nostr.relay_count() == 1, "a duplicate relay must not add a row"
        d.clear_and_type("nostr-relay-input", "")
        nostr.clear_relays()
    finally:
        nostr.navigate()
        if nostr.is_linked() and not was_linked:
            nostr.unlink()
            nostr.wait_for_unlinked()
        elif was_linked and not nostr.is_linked():
            nostr.link_generate()
            nostr.wait_for_linked()
