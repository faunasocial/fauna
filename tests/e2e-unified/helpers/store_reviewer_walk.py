"""A store reviewer's session, walked the way a Google Play, App Store or
Microsoft Store reviewer is likely to walk it — one module, two callers:

* ``tests/test_store_reviewer_journey.py`` (tier_4) on a seeded staging box,
  signing in as the org accounts the provisioning drive minted there;
* ``tests/test_store_reviewer_journey_local.py`` (tier_3) on a standalone nest
  seeded the same way (``helpers.org_account_session.seed_accounts``), so the
  journey runs on every machine's inner loop.

Why it exists: every store reviews a build by signing in to the organisation's
acceptance nest as that store's own reviewer account (`org-nests.md`
§ fauna.build → *Accounts*; `installers/store-review.md` § The tracks). A
reviewer of an app with user content checks first that the app is not empty and
works, then that the safety promises of the certification notes hold
(`terms-and-content-guidelines.md` § Certification notes wording: reporting on
every post, message and profile, blocking any account, the terms in Settings),
and that an account can be deleted from inside the app. The walk follows the
reviewer script of the 2026-10-28 store reviewer walkthrough:

  1. **Sign in on a fresh install** with the reviewer's handle and secret —
     the terms notice and its link read before Continue.
  2. **The first run is not empty**: the tester's seeded posts in the feed,
     the tester among the contacts, the tester's conversation in the list, the
     tester's request to connect among the notifications.
  3. **Messaging works**: the reviewer reads the conversation and replies.
  4. **Posting works**: the reviewer publishes a post of their own.
  5. **Report a message** from its ⋯ menu (beside *Mark as spam*), **a post**
     from its ⋯ menu, and **an account** from its profile; each report is
     acknowledged, listed in the reporter's ledger on the Moderation page, and
     withdrawn there.
  6. **Block an account** from its profile, and unblock it.
  7. **Search** finds the tester's profile.
  8. **Settings**: the terms link, the app version, the nest's name on Status,
     the Sessions page with this app's own row and its lock (read, NEVER
     pressed), the account-deletion section (read, never armed), and Mail &
     Calendar stating the account's mail state.

What it mutates, and what teardown removes (`testing.md` § The shared-box rule,
non-destructive carve-out — what lets the live caller run on a staging box):
the tester posts one run-token post and sends the reviewer one run-token
message — the content the reviewer reports, so a report never lands on the
seeded first-run content: a report hides its subject from the reporter for good
(the reporter-side hide; no app offers an un-hide), which would strip the
reviewer's own seeded feed and conversation. The reviewer replies once and
posts once. Teardown deletes all four through the app's own delete verbs (a
deleted message leaves the product's deletion marker in the thread). All three
reports are withdrawn in the run; a withdrawn report stays in the nest admin's
queue as a withdrawn row (the audit record, `moderation.md` § User-initiated
reporting → Persistence). The block and the account report target ANOTHER
reviewer account, never the tester: unblock clears the contact edge
(`ui/profile.md` § Element IDs, `profile-block-button`), so blocking the tester
would end the seeded contact for good, and a profile report hides everything
that account authored from the reporter (`moderation.md` § Corollary — block
also hides); two reviewers share no edge and no content. Every leg signs in on
a reset app instance and ends signed out (``helpers.org_account_session``), so
the accounts' 3-device free tier is left as found. Never a factory reset of the
nest, and none must be added.
"""

from __future__ import annotations

import uuid
from dataclasses import dataclass, field

from helpers.org_account_session import poll, sign_in_fresh, sign_out
from helpers.staging_box import (
    REVIEWER_ACCOUNTS,
    SEED_MESSAGE,
    SEED_POSTS,
    SEEDING_ACCOUNT,
    actor_hex_for,
)
from i18n.strings import S

#: The accounts the walk needs: the three reviewers and the tester.
ACCOUNTS = (*REVIEWER_ACCOUNTS, SEEDING_ACCOUNT)

#: `fauna_core::TERMS_URL` — the one constant every app's terms link opens; a
#: terminal cannot open a browser, so tui's link shows the URL itself.
TERMS_URL = "https://fauna.social/terms"

# Named budgets — ceilings on deadline polls that return the moment the state
# holds (convention 14), sized for a round trip to a box over the internet.
PAGE_BUDGET_S = 60.0      # a page painting after a nav
SEND_BUDGET_S = 120.0     # one round trip to the nest + the repaint
FEED_BUDGET_S = 120.0     # a post reaching a re-selected feed

#: The words of a request-to-connect notification row, between its sender and
#: its message (`notifications.row_knock`).
_KNOCK_WORDS = (S.notifications.row_knock(sender="\x00", message="\x01")
                .split("\x00", 1)[1].split("\x01", 1)[0].strip(" :"))


@dataclass
class Nest:
    """Where the walk runs. ``where``: the handle domain every account signs
    in under (`<localpart>@<where>`). ``status_domain``: the nest's name on the
    Status page — its `fauna.nest.info` domain. ``report_name``: the nest a
    report's acknowledgement names (the submit reply's routing), or ``None``
    for any nest named — on a box the two names are the box's; a domain-less
    local nest answers `unknown` to the one and routes as `localhost`.
    ``mail_state``: the `state` Mail & Calendar's toggle must read (`on`,
    `off`, `unavailable`), or ``None`` for any state the page states legibly."""
    where: str
    status_domain: str
    report_name: str | None = None
    mail_state: str | None = None


@dataclass
class _Run:
    token: str = field(default_factory=lambda: uuid.uuid4().hex[:10])
    created: dict = field(default_factory=lambda: {
        "tester_post": False, "tester_message": False,
        "own_post": False, "own_reply": False})

    @property
    def tester_post(self) -> str:
        return f"Reviewer run {self.token}: a post from the tester."

    @property
    def tester_message(self) -> str:
        return f"Reviewer run {self.token}: a message from the tester."

    @property
    def own_post(self) -> str:
        return f"Reviewer run {self.token}: the reviewer's own post."

    @property
    def own_reply(self) -> str:
        return f"Reviewer run {self.token}: the reviewer's reply."


def run_reviewer_journey(app, accounts: dict[str, dict], reviewer: str, nest: Nest) -> None:
    """The whole session for one reviewer: the tester leaves fresh content to
    report, the reviewer walks the script, and teardown removes every piece of
    run content and signs out. ``accounts`` maps each of :data:`ACCOUNTS` to
    its identity (``localpart``, ``secret_hex``); ``app`` holds no session."""
    me, tester = accounts[reviewer], accounts[SEEDING_ACCOUNT]
    # Another store's reviewer: the account this one blocks, unblocks and reports.
    other = accounts[next(lp for lp in REVIEWER_ACCOUNTS if lp != reviewer)]
    run = _Run()
    cleanup_failures: list[str] = []
    signed_in = False
    try:
        # ── The tester leaves fresh content to report ─────────────────────
        sign_in_fresh(app, tester, nest.where, signed_in=False)
        signed_in = True
        _enter_feed(app)
        app.feed.create_post(run.tester_post, timeout=SEND_BUDGET_S)
        run.created["tester_post"] = True
        thread = _thread_with(app, lambda t: reviewer in t.label,
                              f"{SEEDING_ACCOUNT}'s conversation with {reviewer} "
                              "(the seeded one)")
        app.conversations.open_thread_by_id(thread.thread_id)
        app.conversations.send_in_open_thread(run.tester_message)
        _bubble_index(app, run.tester_message, f"{SEEDING_ACCOUNT}'s message painting")
        run.created["tester_message"] = True

        # ── 1. The reviewer signs in on a fresh install ────────────────────
        sign_in_fresh(app, me, nest.where, check_terms=True)
        try:
            _walk_as_reviewer(app, reviewer, other, run, nest)
        finally:
            _remove_reviewer_content(app, run, cleanup_failures)
    finally:
        if run.created["tester_post"] or run.created["tester_message"]:
            _best_effort("signing the tester back in for teardown",
                         lambda: sign_in_fresh(app, tester, nest.where, signed_in=signed_in),
                         cleanup_failures)
            _remove_tester_content(app, run, cleanup_failures)
        if signed_in:
            _best_effort("signing out", lambda: sign_out(app), cleanup_failures)
    assert not cleanup_failures, (
        f"the journey passed but its teardown left run content on {nest.where}: "
        f"{cleanup_failures} — remove it by hand"
    )


def _walk_as_reviewer(app, reviewer, other, run: _Run, nest: Nest) -> None:
    note = f"store reviewer journey {run.token}"

    # ── 2. The first run is not empty ────────────────────────────────────
    _enter_feed(app)
    for text in (*SEED_POSTS, run.tester_post):
        assert app.feed.wait_for_post_text(text, timeout_s=FEED_BUDGET_S), (
            f"{reviewer}'s feed should show the tester's post {text!r}; "
            f"error={app.error_text()!r}"
        )
    app.contacts.navigate()
    poll(lambda: (any(SEEDING_ACCOUNT in n for n in app.contacts.contact_names()),
                  f"{app.contacts.contact_names()} error={app.error_text()!r}"),
         f"{reviewer} holding {SEEDING_ACCOUNT} as a contact", PAGE_BUDGET_S, interval=2.0)
    _assert_knock_notification(app, reviewer)

    # ── 3. Messaging: read the tester's conversation, reply ──────────────
    thread = _thread_with(app, lambda t: run.tester_message in t.snippet,
                          f"{reviewer} seeing {SEEDING_ACCOUNT}'s conversation")
    app.conversations.open_thread_by_id(thread.thread_id)
    _bubble_index(app, SEED_MESSAGE, f"{reviewer} reading the seeded message")
    app.conversations.send_in_open_thread(run.own_reply)
    _bubble_index(app, run.own_reply, f"{reviewer}'s reply painting")
    run.created["own_reply"] = True

    # ── 5a. Report the tester's message from its ⋯ menu ──────────────────
    app.conversations.open_message_actions(
        message_index=_bubble_index(app, run.tester_message, "the message to report"))
    _assert_menu_item(app, "dm-message-report-button",
                      S.conversations.detail.report_message, "the message ⋯ menu")
    _assert_menu_item(app, "dm-message-mark-as-spam-button",
                      S.conversations.detail.mark_as_spam, "the message ⋯ menu")
    app.report.open_from_message_menu()
    _report(app, "spam", note, "reporting a message", nest)

    # ── 4. Post something of their own ───────────────────────────────────
    _enter_feed(app)
    app.feed.create_post(run.own_post, timeout=SEND_BUDGET_S)
    run.created["own_post"] = True

    # ── 5b. Report the tester's post from its ⋯ menu ─────────────────────
    _enter_feed(app)
    assert app.feed.wait_for_post_text(run.tester_post, timeout_s=FEED_BUDGET_S)
    app.feed.open_post_actions(app.feed.post_index_by_text(run.tester_post))
    _assert_menu_item(app, "feed-post-report-button", S.feed.report_post, "the post ⋯ menu")
    # The rest of `ReportActions.open_from_post`, the menu being open already.
    app.driver.click("feed-post-report-button")
    app.driver.wait_for("report-sheet", timeout=10)
    _report(app, "spam", note, "reporting a post", nest)

    # ── 6. Block another account from its profile, then unblock ──────────
    app.profile.require_state_protocol_actor_nav_supported()
    app.profile.navigate_to_actor(actor_hex_for(other["secret_hex"]))
    app.profile.block()
    assert app.profile.wait_for_block_label("Unblock", timeout=SEND_BUDGET_S), (
        f"{reviewer} blocking {other['localpart']}: label {app.profile.block_label()!r} "
        f"error={app.error_text()!r}"
    )
    app.profile.block()
    assert app.profile.wait_for_block_label("Block", timeout=SEND_BUDGET_S), (
        f"{reviewer} unblocking {other['localpart']}: label {app.profile.block_label()!r} "
        f"error={app.error_text()!r}"
    )

    # ── 5c. Report that account from its profile ─────────────────────────
    app.report.open_from_profile()
    _report(app, "spam", note, "reporting an account", nest)

    # ── 5d. All three reports on the ledger; withdrawn ───────────────────
    app.moderation.navigate()
    poll(lambda: (app.moderation.withdraw_count() >= 3, app.moderation.report_rows()),
         f"{reviewer}'s three open reports on the Moderation ledger", PAGE_BUDGET_S,
         interval=2.0)
    for _ in range(3):
        before = app.moderation.withdraw_count()
        # Newest first, and each open row carries its own withdraw: index 0 is
        # this run's newest open report.
        app.moderation.withdraw_report(0)
        poll(lambda before=before: (app.moderation.withdraw_count() == before - 1,
                                    app.moderation.report_rows()),
             f"{reviewer}'s report withdrawn", SEND_BUDGET_S, interval=1.0)

    # ── 7. Search finds the tester ───────────────────────────────────────
    app.search.navigate()
    app.search.query(SEEDING_ACCOUNT)
    poll(lambda: (any(SEEDING_ACCOUNT in t for t in app.search.result_texts()),
                  f"{app.search.result_texts()} error={app.error_text()!r}"),
         f"{reviewer}'s search for {SEEDING_ACCOUNT!r} listing that profile",
         SEND_BUDGET_S, interval=2.0)

    # ── 8. Settings: terms, version, the nest's name, Sessions, deletion, mail ─
    app.settings.navigate_to_about()
    assert app.driver.is_visible_scrolled("settings-terms-link"), (
        f"the terms link in Settings: {app.driver.diagnose('settings-terms-link')}"
    )
    terms = app.get_text("settings-terms-link")
    assert S.settings.terms_link in terms, f"the Settings terms link reads {terms!r}"
    if app.driver.is_tui():
        assert TERMS_URL in terms, (
            f"a terminal shows the terms URL itself; the link reads {terms!r}"
        )
    assert app.settings.app_version(), "Settings must say which app version is running"
    if not app.driver.is_visible_scrolled("status-node-domain"):
        # tui paints Status on the Settings root, where the About block is;
        # the sidebar shells keep it behind its own rail row.
        app.settings.open_rail_row_by_click("settings-nav-row[status]", "status-node-domain",
                                            PAGE_BUDGET_S)
    poll(lambda: (app.driver.get_text("status-node-domain").strip() == nest.status_domain,
                  app.driver.get_text("status-node-domain")),
         f"the Status page naming {nest.status_domain}", PAGE_BUDGET_S, interval=1.0)

    app.sessions.navigate(timeout=PAGE_BUDGET_S)
    poll(lambda: (any(app.sessions.mark(i) for i in range(app.sessions.card_count())),
                  f"{app.sessions.card_count()} cards error={app.error_text()!r}"),
         "Sessions listing this app's own session, marked", PAGE_BUDGET_S, interval=2.0)
    assert app.sessions.lockout_warning().strip(), "the Sessions lock states its warning"
    assert not app.sessions.lock_enabled(), (
        "the lock must stay disarmed until its confirm is typed — this test never types it"
    )
    assert app.settings.delete_confirm_text().strip(), (
        "Settings → Account must offer account deletion with its confirmation text"
    )
    assert not app.settings.is_delete_account_enabled(), (
        "account deletion must stay disarmed until DELETE is typed — this test never types it"
    )
    _assert_mail_state(app, reviewer, nest)


def _assert_knock_notification(app, reviewer) -> None:
    """The tester's request to connect is among the reviewer's notifications
    (read, nothing pressed)."""
    def _listed():
        app.notifications.navigate()
        texts = [app.driver.get_text("notification-item", index=i)
                 for i in range(app.notifications.notification_count())]
        return any(_KNOCK_WORDS in t for t in texts), f"{texts} error={app.error_text()!r}"

    poll(_listed, f"{reviewer}'s notifications listing {SEEDING_ACCOUNT}'s request to "
         "connect", PAGE_BUDGET_S, interval=2.0)


def _assert_menu_item(app, element_id: str, label: str, where: str) -> None:
    app.driver.wait_for(element_id, timeout=10)
    text = app.driver.get_text(element_id)
    assert label in text, f"{where} must carry {label!r}; {element_id} reads {text!r}"


def _assert_mail_state(app, reviewer, nest: Nest) -> None:
    """Settings → Mail & Calendar states the account's mail state in words a
    reviewer can act on: on, off with the enable explanation, or unavailable
    with its notice (`ui/mail-settings.md` § Layout & flow). A staging box runs
    with `mail.auto_enable_for_new_users` at its default, on
    (`behavior/mail-policy-config.md`), so an admitted account has mail there."""
    app.settings.open_rail_row_by_click("settings-nav-row[mail-settings]",
                                        "mail-settings-enabled-toggle", PAGE_BUDGET_S)
    drv = app.driver
    state = drv.get_attr("mail-settings-enabled-toggle", "state") or ""
    assert state in ("on", "off", "unavailable"), (
        f"Mail & Calendar's toggle reads no mail state ({state!r}); "
        f"{drv.diagnose('mail-settings-enabled-toggle')}"
    )
    if state == "unavailable":
        notice = drv.get_text("mail-settings-unavailable-notice")
        assert S.mail_settings.unavailable_notice in notice, (
            f"mail is unavailable on {nest.where}, but the page's notice reads {notice!r}"
        )
    if nest.mail_state is not None:
        assert state == nest.mail_state, (
            f"{reviewer}'s Mail & Calendar reads {state!r} on {nest.where}, expected "
            f"{nest.mail_state!r} — a reviewer told 'messages and mail, one inbox' "
            "meets a different state (a finding for the store reviewer walkthrough)"
        )


def _thread_with(app, pred, what: str):
    """Poll the conversation list until a thread satisfies ``pred``."""
    found: dict = {}

    def _seen():
        threads = app.conversations.list_threads()
        hit = next((t for t in threads if pred(t)), None)
        found["t"] = hit
        return hit is not None, [(t.thread_id, t.label, t.rail, t.participant_count, t.snippet)
                                 for t in threads]

    app.conversations.navigate()
    poll(_seen, what, SEND_BUDGET_S, interval=2.0)
    return found["t"]


def _bubble_index(app, needle: str, what: str) -> int:
    """The index of the painted bubble carrying ``needle`` — also its
    ``dm-message-actions-button`` index."""
    d = app.driver
    texts: dict = {}

    def _painted():
        texts["all"] = [d.get_text("dm-message-text", index=i)
                        for i in range(d.count("dm-message-text"))]
        return any(needle in t for t in texts["all"]), f"{texts['all']} error={app.error_text()!r}"

    poll(_painted, what, SEND_BUDGET_S, interval=1.0)
    return next(i for i, t in enumerate(texts["all"]) if needle in t)


def _bubble_gone(app, needle: str) -> tuple[bool, str]:
    """Whether no painted bubble carries ``needle`` — with what IS painted
    (each bubble's text and whether it is a tombstone), so a red says whether
    the delete never landed or a second copy of the message survived it."""
    d = app.driver
    texts = [d.get_text("dm-message-text", index=i) for i in range(d.count("dm-message-text"))]
    hits = sum(needle in t for t in texts)
    return hits == 0, (f"{hits} painted bubble(s) still carry it; painted={texts} "
                       f"tombstones={d.count('dm-message-deleted')} "
                       f"error={app.error_text()!r}")


def _report(app, reason: str, note: str, what: str, nest: Nest) -> None:
    """Fill the open report sheet as a reviewer would and send it; never ticks
    "block the author" (a block of the tester would end the seeded contact)."""
    app.report.choose_reason(reason)
    app.report.fill_note(note)
    assert not app.report.block_author_checked(), (
        f"{what}: the report sheet must not block the author unless asked"
    )
    poll(lambda: (app.report.submit_enabled(), app.error_text()), f"{what}: sendable",
         PAGE_BUDGET_S, interval=0.5)
    app.report.submit()
    poll(lambda: (app.report.has_status(), app.error_text()), f"{what}: acknowledged",
         SEND_BUDGET_S, interval=1.0)
    ack = app.report.status_text()
    if nest.report_name is not None:
        assert nest.report_name in ack, (
            f"{what}: the acknowledgement must name {nest.report_name}, got {ack!r}"
        )
    else:
        sent = S.moderation.report.sent_local(nest="\x00").split("\x00", 1)[0]
        named = ack.split(sent, 1)[1].strip(" .") if ack.startswith(sent) else ""
        assert named, f"{what}: the acknowledgement must name a nest, got {ack!r}"


def _enter_feed(app) -> None:
    """Enter the Feed page as a reviewer does — its default feed, no named feed
    picked (a real account's Feed page paints no fixture-named feed) — through a
    real page entry, so it re-queries the nest (`FeedActions.reenter`)."""
    app.feed.navigate()
    app.feed.reenter()


def _best_effort(what: str, fn, failures: list[str]) -> None:
    """A teardown step: run it, and record — never raise — a failure, so one
    failed cleanup neither masks the test's own failure nor skips the rest."""
    try:
        fn()
    except Exception as e:  # noqa: BLE001 — teardown must not mask the test's outcome
        failures.append(f"{what}: {e!r}")


def _remove_reviewer_content(app, run: _Run, failures) -> None:
    if run.created["own_post"]:
        def _delete_post():
            _enter_feed(app)
            app.feed.delete_post(app.feed.post_index_by_text(run.own_post),
                                 timeout_s=SEND_BUDGET_S)
            run.created["own_post"] = False
        _best_effort("deleting the reviewer's post", _delete_post, failures)
    if run.created["own_reply"]:
        def _delete_reply():
            _open_thread_carrying(app, run.own_reply)
            app.conversations.delete_message(
                _bubble_index(app, run.own_reply, "the reply to delete"),
                timeout_s=SEND_BUDGET_S)
            poll(lambda: _bubble_gone(app, run.own_reply),
                 "the reviewer's reply deleted", SEND_BUDGET_S, interval=1.0)
            run.created["own_reply"] = False
        _best_effort("deleting the reviewer's reply", _delete_reply, failures)


def _remove_tester_content(app, run: _Run, failures) -> None:
    if run.created["tester_post"]:
        def _delete_post():
            _enter_feed(app)
            assert app.feed.wait_for_post_text(run.tester_post, timeout_s=FEED_BUDGET_S)
            app.feed.delete_post(app.feed.post_index_by_text(run.tester_post),
                                 timeout_s=SEND_BUDGET_S)
            run.created["tester_post"] = False
        _best_effort("deleting the tester's post", _delete_post, failures)
    if run.created["tester_message"]:
        def _delete_message():
            _open_thread_carrying(app, run.tester_message)
            app.conversations.delete_message(
                _bubble_index(app, run.tester_message, "the tester's message to delete"),
                timeout_s=SEND_BUDGET_S)
            poll(lambda: _bubble_gone(app, run.tester_message),
                 "the tester's message deleted", SEND_BUDGET_S, interval=1.0)
            run.created["tester_message"] = False
        _best_effort("deleting the tester's message", _delete_message, failures)


def _open_thread_carrying(app, needle: str) -> None:
    """Open the thread whose latest message is ``needle``, else the first
    thread that paints it."""
    _thread_with(app, lambda t: True, "a conversation list")
    candidates = app.conversations.list_threads()
    ordered = sorted(candidates, key=lambda t: needle not in t.snippet)
    for t in ordered:
        app.conversations.open_thread_by_id(t.thread_id)
        d = app.driver
        if any(needle in d.get_text("dm-message-text", index=i)
               for i in range(d.count("dm-message-text"))):
            return
    raise AssertionError(f"no conversation paints {needle!r}: "
                         f"{[(t.label, t.snippet) for t in candidates]}")
