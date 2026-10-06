"""tier_3: the S3/S4 rollout's owed e2e-unified journey — mail arrives, then the
Search page finds it through the LOCAL sealed content index (backend 2), driven
through the real tui app UI rather than the Rust-internal `SearchManager`
pin (`bins/fauna-nest/tests/conformance_index_rail_publisher.rs`).

Why a search hit here can only come from backend 2 (never backend 1): the nest's
`content_fts` table (`docs/goal/behavior/content-index.md` § Two search
backends today) is fed only by public post bodies, profile handles/bios, and the
policy-gated public bridge corpus — never a user's private mail. A search hit on
this test's private, freshly-delivered nonce is therefore proof that the whole
client-side pipeline ran for real: the receive hook fired `observe_for_index`
(`fauna_conversations::index_sink`), `IndexBuilder` staged + sealed +
published a segment, and the `SearchManager`'s local arm
(`fauna_client_index::MailLocalSearch`) queried it and merged the row into the
page.

Delivery mechanics mirror `test_mail_client_receive.py` (the real Go MTA bridge,
real SMTP wire, real HPKE seal, real client-side open) — this test picks up
where that one stops (proving the mail *arrived*) and continues into Search.

**This test found a real product gap, and the gap is CLOSED (2026-08-05) — the
product was fixed, not the test.** As written it enables mail AFTER login,
exactly the proven pattern `test_mail_client_receive.py` uses for the RECEIVE
path. Receiving tolerated that because `MailKeyCache` re-checks lazily on every
poll; indexing did not, because `IndexBuilderLauncher::launch()` was a ONE-SHOT
check at `ConversationsSession::start_receive_loop`'s prologue — it had already
run and found no MSEK before mail became enabled, so the mail-index observer
never registered for the rest of that process's life. The mail arrived and
rendered in Conversations fine, and was silently never staged into the local
search index; only an app restart recovered it.

The fix is the indexing twin of the receive path's own lazy re-check:
`IndexBuilderLauncher::ensure_arm(kind)`, asked by the receive loop on every
sweep of that kind, **before** the poll, attaching into the one registered
observer container rather than replacing it — and giving the late arm a fresh
catch-up window, since the mail boundary closes on sweep 1 while mail is still
disabled. `docs/goal/behavior/content-index.md` § Ingest triggers, v1 → *An arm
attaches when its precondition arrives* owns the rule; its tier_1 twin is
`libs/fauna-conversations/tests/index_catch_up_boundary_tests.rs`, which can see
what an end-state search assertion cannot (that the running arm was left
undisturbed across the attach).

So this test now proves the mid-session-enablement path for real, which is
strictly more than the mainline path it would have proven had it been rewritten
to enable mail before login.

Test taxonomy:
- `tier_3` (mocking depth): every binary real, real SMTP wire, real
  MTA-seal -> client receive -> client-side index build/publish -> Search query.

tui + linux: the local search arm's query side is registered on both as of
2026-08-10 (`docs/goal/ui/search.md` § Implementation status today — linux's leg, `conv_backend.rs`
registers the resolver right where the AuthSuccess arm builds the index
launcher, the same synchronous-registration shape tui's `attach_local_index`
uses). The other apps adopt the shared `SearchManager` in follow-on slices.
"""

import time

import pytest

from helpers.app_surface import app_name, skip_unbuilt
from helpers.mail_wire import _connect_smtp_starttls
from helpers.waiting import wait_until
from helpers.mail_aliases import add_exact_alias

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.windows,
    pytest.mark.real_conversations,
]

# The (external) sender's domain — has no local-domain / loopback exemption, so
# the message arrives as genuine external inbound.
SENDER_DOMAIN = "external.test"


def _deliver_inbound(mx_port: int, server_name: str, recipient_addr: str,
                     raw_message: bytes, deadline: float) -> None:
    """Drive one real inbound SMTP MAIL/RCPT/DATA transaction through the MTA's
    port-25 STARTTLS listener. Returns after the `250` on `.`, which the MTA
    sends only once the WS-RPC `ingest_inbound_mail` (seal + store) committed."""
    with _connect_smtp_starttls(mx_port, server_name, deadline) as conn:
        conn.cmd(f"MAIL FROM:<sender@{SENDER_DOMAIN}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("private-search-index")
def test_received_mail_is_found_by_search_via_the_local_index(
    logged_in_app, mail_bridge_mta, nest_instance, test_user
):
    app = logged_in_app

    # ── 1. Enable mail + register the inbound routing alias — identical setup
    # to test_mail_client_receive.py (see that file's steps 1-2 for the full
    # rationale: MailSettingsMachine mints the MSEK and registers the
    # MSEK-derived recipient pubkey; the alias is what validate_recipient
    # resolves on RCPT TO).
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    domain = mail_bridge_mta.domain
    local_part = "e2esearch"
    recipient_addr = f"{local_part}@{domain}"
    add_exact_alias(nest_instance["url"], test_user["signing_key"], domain, local_part)

    # ── 2. Deliver a real inbound email carrying a needle unique to this run —
    # content_fts (backend 1) can never contain it (private mail is never in
    # that corpus), so any search hit on it necessarily came from the local
    # sealed index (backend 2).
    needle = f"localidx{int(time.time() * 1000)}qx"
    subject = f"Local index search {needle}"
    message_id = f"<{needle}@{SENDER_DOMAIN}>"
    body_lines = [
        f"From: External Sender <sender@{SENDER_DOMAIN}>",
        f"To: {recipient_addr}",
        f"Subject: {subject}",
        f"Message-ID: {message_id}",
        "Date: Mon, 25 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"The {needle} report is attached for your review.",
    ]
    raw_message = ("\r\n".join(body_lines) + "\r\n").encode()
    _deliver_inbound(
        mail_bridge_mta.mx_port, domain, recipient_addr, raw_message,
        time.monotonic() + 40.0,
    )

    # ── 3. Wait for the shared receive loop to fetch + decrypt + ingest —
    # proves the mail arrived before asserting anything about search, so a
    # search miss cannot be misdiagnosed as a delivery failure.
    def find_received():
        for t in app.conversations.list_threads():
            if needle in (t.label or "") or needle in (t.snippet or ""):
                return t
        return None

    received = wait_until(
        find_received,
        60.0,
        diagnose=lambda: (
            f"the inbound email tagged {needle!r} never surfaced in the "
            f"conversations list — search cannot be tested if the mail never "
            f"arrived. conversations error: {app.error_text()!r}; "
            f"bridge log: {mail_bridge_mta.log_file}"
        ),
    )
    assert received is not None

    # ── 4. The content-index builder observes the same ingest and flushes on a
    # debounce timer, not instantly — poll Search rather than asserting once.
    # The worst-case latency is the periodic flush backstop
    # (`libs/fauna-client-index/src/lifecycle.rs::PERIODIC_INTERVAL`, 60s), not
    # just the 5s stage-quiescence debounce, so the poll window must clear it
    # with margin.
    app.search.navigate()

    def query_and_check():
        app.search.query(needle)
        return app.search.result_count() > 0

    found_in_search = wait_until(
        query_and_check,
        90.0,
        diagnose=lambda: (
            f"mail tagged {needle!r} was received but never found by Search — "
            f"the local content-index pipeline (receive hook -> IndexBuilder "
            f"stage/flush -> __index publish -> SearchManager local arm) did not "
            f"deliver it end to end. search error: {app.error_text()!r}; "
            f"no-results shown: {app.search.has_no_results()}"
        ),
    )
    assert found_in_search
    result_text = app.search.result_text(0)
    assert needle in result_text, (
        f"the search result row must render the matched mail's content; "
        f"got {result_text!r} for needle {needle!r}"
    )

    # ── 5. Activating the row lands BOTH halves of `SearchNav::Mail`'s contract
    # — "open the thread AND select this message in it" (ui/search.md § State &
    # data shape; conversations.md § The selected message). Before 2026-08-10 no
    # app had a seam for the second half at all, so the `message_id` was carried
    # and dropped; this is the leg that proves it is acted on.
    app.search.open_result(0)
    app.conversations.driver.wait_for("thread-header", timeout=15.0)

    # The shared half (`select_thread_and_message` + the resolved
    # `ThreadDetail.selected_message_id`) is done for all seven apps; what the
    # remaining apps owe is the paint + the `selected` attribute. Declared as
    # unbuilt debt rather than skipped silently, so `--strict-app` reds it and
    # the run tallies it (e2e convention 7). linux painted it 2026-08-15, apple's macOS leg 2026-08-25, windows registered its local arm 2026-08-31
    # (its paint — `DmMessageBubble.xaml.cs`'s `selected` attribute + amber
    # ring + `StartBringIntoView` — landed earlier, in row 84/88; only the
    # seeding precondition was missing) — this pytestmark's `tui`/`linux`/`macos`/`windows` are the apps
    # this specific test can exercise TODAY (android still owes the paint;
    # web's local arm never will exist — no Tantivy on wasm, structurally, so
    # `SearchNav::Mail` can never reach web in production; iOS has the paint
    # too, but a single-seat run can never seed its OWN local index —
    # `CLIENT_BUILDS_INDEX` is `false` there — so this test can never even
    # reach step 5 on iOS regardless of the paint; grade iOS on a real
    # simulator instead, per `app_surface.py`'s phone-seat declared-absence).
    if app_name(app.driver) not in ("tui", "linux", "macos", "windows"):
        skip_unbuilt(
            app.conversations.driver,
            surface="the `selected` attribute on dm-message-timestamp",
            detail=(
                "SearchNav::Mail's message-selection half is built on tui (lead "
                "app, 2026-08-10) and linux (2026-08-15); the shared state is "
                "already in ConversationsManager, so an app that registers its "
                "local search arm owes only the paint"
            ),
            tracked="conversations.md § The selected message; the per-app trickle-down NEXTs",
        )

    selected = app.conversations.selected_message_index()
    assert selected is not None, (
        f"activating the mail result opened the thread but marked no message — "
        f"`SearchNav::Mail`'s message_id was dropped. "
        f"{app.conversations.driver.diagnose('dm-message-timestamp')}; "
        f"conversations error: {app.error_text()!r}"
    )

    # The load-bearing assertion is not "something is marked" but "the marked
    # message is the one the row named" — a marker stuck on bubble 0 would pass
    # the check above on any thread whose hit happens to be first.
    marked_text = app.conversations.driver.get_text("dm-message-text", index=selected)
    assert needle in marked_text, (
        f"the marked message must be the one the search row named: expected the "
        f"bubble carrying {needle!r}, but bubble {selected} reads {marked_text!r}"
    )
