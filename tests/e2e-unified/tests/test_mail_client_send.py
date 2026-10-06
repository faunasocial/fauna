"""tier_3: the SEND half of the single-user mail round-trip, driven through
the Fauna APP UI (not raw SMTP submission).

The client composes a new conversation to an external recipient and clicks
Send; the shared `SmtpBackend` (`libs/fauna-conversations`) assembles the
RFC 5322 message and submits it over the `fauna.email.send` WS-RPC kind to the
nest, which enqueues the external recipient on the outbound queue; the MTA's
outbound worker relays it to the stub external MX (reached via
`mta_mx_override`). Asserting the stub captured the composed message proves the
client-driven outbound path end-to-end — the counterpart to
`test_mail_bridge_mta.py::test_submission_round_trip`, which drives the same
relay via a raw third-party-MUA SMTP submission rather than the Fauna app.

Native Fauna apps send mail over WS-RPC, never as third-party SMTP MUAs
(those wrapped-credential MUA logins are reserved for Thunderbird/Apple Mail —
`docs/goal/behavior/mail-credentials.md`).

Scope: client-driven SEND is wired on linux, web, windows, apple (green
`--client macos`/`ios` via the shared conversations compose UI + `SmtpBackend`
under the real-conversations harness) and android — `ConversationsManagerHost.
startConversationsSession`'s `conversations_session` factory wires the SMTP
send sink the same as every other platform (landed 2026-07-20 alongside
android's own `FAUNA_E2E_REAL_CONVERSATIONS` launch-gate twin), and the
compose bar's `Ids.RECIPIENT_PICKER_INPUT`/`DM_TEXT_FIELD`/`DM_SEND_BUTTON`
match ui.yaml exactly. The RECEIVE half through the client
(received mail rendered in the conversations list) is the symmetric twin, green in
`test_mail_client_receive.py` (incl. windows + macos + ios + android) — together
they prove the full client-driven send+receive round-trip.

Windows rides the real-conversations harness like the rest of the
`dedicated_mail_nest` family: the `real_conversations` marker makes the windows
driver build the real `ConversationsSession` (with the SMTP send rail registered
by the FFI `conversations_session` factory, `libs/fauna-ffi/src/nest_client.rs`)
instead of the mock backend host; the compose UI + `send_new_thread` path is
production-identical to linux/web (no per-app send code). apple has the same
harness (FaunaMacApp/FaunaApp `applySessionPatch`); `.macos` is now gated (green in
a co-running `--client macos` session). The apple mail markers were previously held
on a wrong "receive loop doesn't drain after a per-module relaunch" theory — the
real defect was a sibling spam test leaving a full-confidence spam model in the
session-shared nest, fixed by its `_isolate_spam_model` teardown; send itself never
scored, so it always passed. `.ios` is gated too (green in a co-running
`--client ios` session).
"""

import time

import pytest

from i18n.strings import S

# Client-driven SMTP send is wired on linux + web + windows (apple/android
# compose-send is the shared-Rust conversations-rails follow-up). Mark the supported clients so
# --client deselects this on others rather than building the mail bridge and then
# skipping in-body (the build is session-scoped + slow — it was blowing the
# per-test timeout on a windows full-suite run).
# real_conversations: windows builds the real ConversationsSession (SMTP rail
# registered) instead of the mock host only when a collected test carries this
# marker (conftest sets FAUNA_E2E_REAL_CONVERSATIONS=1 for the windows config);
# linux/web always run the real session under e2e, so the marker is a no-op there.
# reclaim_cycle: the mail area's representative in the post-reclaim gate — under
# `--reclaim-cycle` the shared nest is wiped + re-claimed before this runs, so the
# mail bridge re-enrolls and the client-driven outbound path is exercised
# post-reclaim. See `just e2e-reclaim-cycle-test`.
pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.web,
              pytest.mark.windows, pytest.mark.macos, pytest.mark.ios,
              pytest.mark.tui, pytest.mark.android,
              pytest.mark.real_conversations, pytest.mark.reclaim_cycle]

# The relayed-to domain; the `mail_bridge_mta` fixture routes it to the
# in-process stub MX via the `mta_mx_override` operator hatch.
EXTERNAL_DOMAIN = "external.test"


def _find_in_stub(stub_mx, token: str):
    """First stub-MX message whose bytes carry `token` (in the Subject), or None."""
    for raw in stub_mx.messages():
        if token.encode() in raw:
            return raw
    return None


def _assert_from_is_the_sender(received: bytes, test_user):
    """The delivered bytes must carry the sender's OWN `From:`.

    Asserting only that the *recipient* appears (which is all this file did
    until 2026-07-31) cannot tell a correct send from two broken ones that both
    "relay": a `From:` the account does not own, and — the one that shipped on
    six apps — an EMPTY `From:`. The empty one is the worse failure and the
    harder to notice: `email_handlers.rs`'s sender-handle gate keys on
    `from_addr.rsplit_once('@')`, so a From with no `@` skips the check
    entirely and a malformed RFC 5322 message goes out to a real MX.
    """
    handle = test_user["handle"]
    assert handle, "the e2e actor must be admitted under a handle to send mail"
    expected = f"From: {handle}@".encode()
    assert expected in received, (
        f"the relayed message must carry the sender's own From: (expected a line "
        f"starting {expected!r}); an absent or foreign From means the send either "
        f"claimed an address this account does not own, or evaded the nest's "
        f"sender-handle gate with an empty one. Got headers: "
        f"{received.split(b'@@@@')[0][:400]!r}"
    )


@pytest.mark.feature("email-in-conversations", "replies-and-threads")
def test_client_driven_send_relays_to_external_mx(logged_in_app, mail_bridge_mta, test_user):
    app = logged_in_app

    token = f"client-send-{int(time.time() * 1000)}"
    recipient = f"recipient@{EXTERNAL_DOMAIN}"
    subject = f"Client-driven outbound {token}"
    body = "Hello from the Fauna conversations compose bar.\n"

    # Compose + send through the conversations UI: new-conversation-button →
    # recipient chip → topic (subject) → body → dm-send-button. The send button
    # routes through manager.send_new_thread() → SmtpBackend::send →
    # fauna.email.send (WS-RPC) on the tokio runtime.
    app.conversations.start_new_conversation(recipient, subject=subject, body=body)

    # The nest enqueues the external recipient on the outbound queue and pushes
    # `fauna.bridges.outbound_ready` to the MTA, which Trigger()s its outbound
    # worker to drain and relay to the stub MX — promptly, not on the next poll.
    #
    # `fauna.email.send` is handled by the *nest* (it writes the
    # `outbound_mail_queue` row), so unlike a raw SMTP submission it can't call
    # the worker's Trigger() in-process. Slice 4b closed that gap: the nest
    # emits a `fauna.bridges.outbound_ready` push to the approved MTA bridge on
    # the enqueue, and the bridge's push dispatcher fires Trigger()
    # (nest mail deploy-verify work § Slice 4b). So the relay is prompt; the
    # worker's 30 s `fetch_outbound_due` poll is only the best-effort backstop.
    # The 20 s deadline is deliberately under that poll cadence — it proves the
    # push path delivered (a poll-only relay would need ≥30 s). The loop returns
    # as soon as the message lands, so a healthy run is a few seconds.
    deadline = time.monotonic() + 20.0
    received = None
    while time.monotonic() < deadline:
        received = _find_in_stub(mail_bridge_mta.stub_mx, token)
        if received is not None:
            break
        time.sleep(0.5)

    assert received is not None, (
        f"stub external MX received no message tagged {token!r} within 20s — "
        f"the client-driven send did not relay out (outbound_ready push path). "
        f"conversations error: {app.error_text()!r}; "
        f"bridge log: {mail_bridge_mta.log_file}"
    )
    assert recipient.encode() in received, (
        "the relayed message must carry the composed recipient in its headers"
    )
    _assert_from_is_the_sender(received, test_user)


@pytest.mark.feature("email-in-conversations")
def test_client_send_without_committing_chip_still_relays(logged_in_app, mail_bridge_mta, test_user):
    """Regression for the live "I click Send and nothing happens" bug.

    The companion test above commits the recipient chip (Enter / suggestion)
    before sending — which masked the real-user flow where someone types an
    address and clicks Send *without* committing a chip. With no committed chip,
    `send_new_thread` used to see `chips.is_empty()` and return `Ok(None)` — a
    silent no-op the linux `on_send` handler swallowed to stderr. The Send action
    now flushes the pending recipient first (`ConversationsManager::send_new_thread`),
    so the message still relays. `commit_chip=False` drives exactly that flow.
    """
    app = logged_in_app

    token = f"no-chip-send-{int(time.time() * 1000)}"
    recipient = f"recipient@{EXTERNAL_DOMAIN}"
    subject = f"No-chip-commit outbound {token}"
    body = "Sent without committing the recipient chip.\n"

    app.conversations.start_new_conversation(
        recipient, subject=subject, body=body, commit_chip=False
    )

    deadline = time.monotonic() + 20.0
    received = None
    while time.monotonic() < deadline:
        received = _find_in_stub(mail_bridge_mta.stub_mx, token)
        if received is not None:
            break
        time.sleep(0.5)

    assert received is not None, (
        f"stub external MX received no message tagged {token!r} within 20s — the "
        f"send-without-chip-commit flow silently dropped the message (the live "
        f"'click Send, nothing happens' bug). conversations error: {app.error_text()!r}; "
        f"bridge log: {mail_bridge_mta.log_file}"
    )
    assert recipient.encode() in received, (
        "the flushed recipient must carry through to the relayed message headers"
    )
    _assert_from_is_the_sender(received, test_user)


# Per-function markers (not the module pytestmark above) so this doesn't widen to
# every app the file's other tests cover — this one needs no mail permission at
# all (the refusal is local), while the sibling relay tests do.
#
# Originally web-only, pinning a regression where the wasm conversations error
# paths rejected with Rust's Debug formatting instead of Display, so
# `error-message` rendered literal Rust enum syntax, e.g. `Other("...")`.
# Widened to tui 2026-07-30: the surface it asserts on is shared, not per-app —
# `conversations.md` § Errors & edge cases has every app render the snapshot
# error when set and fall back to `send_state`, and since the send failure became
# a `LocalizedText` all 7 resolve the same `conversations.unified.error_send`
# template. tui is the lead app, so it carries the second arm.
#
# ✅ GREEN ON WEB since 2026-08-01. It was red for two sessions against a
# PRODUCT THAT WAS ALWAYS CORRECT — the refusal rendered fine and the harness
# could not see it. Root cause (worth keeping, because the class is general):
# `ActionLayer.error_text()` reads the state protocol first and falls back to the
# DOM `error-message` element only when `messages` is None. `/app/conversations`
# is NOT one of `+layout.svelte`'s bare-slot routes, so `<MessageBanner />` IS
# mounted, so the agent reported a present-but-all-null `messages` — which maps
# to "" (not None) and short-circuited that fallback permanently. The page's own
# page-level error is a DIFFERENT element that merely shares the `error-message`
# id, so every conversations-page error was invisible. Fixed in `agent.js`: an
# empty banner reports `messages: null` and defers to the DOM.
#
# ⚠ Two measurements from the earlier diagnosis were WRONG — do not build on them:
#   - "a 1.4 MB body is comfortably UNDER the 1,500,000-byte ceiling" is FALSE.
#     `rfc5322::build_message` emits the body TWICE (the text/plain + text/html
#     multipart/alternative parts), so raw ≈ 2× body: a 1,400,000-byte body
#     assembles to 2,800,582 bytes and is well OVER the ceiling. Both "cases" of
#     the supposed wider bug were the same over-ceiling path.
#   - "any megabyte-scale send on web hangs forever" is FALSE. Measured ladder,
#     `--app web`: 1 KB / 200 KB / 500 KB bodies all RELAY in ~1.5 s (500 KB
#     assembles to ~1.00 MB, under the ceiling); only bodies whose assembled raw
#     exceeds the ceiling took the refusal branch, and only that branch was
#     invisible. The wire is healthy at 1 MB, so the "nest drops an oversized
#     frame" theory explains nothing here.
# The effective user-facing body limit is therefore ~750 KB, not 1.5 MB — a
# consequence of the doubling above, not a defect of this path.
@pytest.mark.tier_3
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.feature("email-in-conversations")
def test_client_send_over_inline_ceiling_refused_with_localized_display_text(
    logged_in_app, mail_bridge_mta
):
    """The message_too_large inline-ceiling pre-check (`SmtpBackend::send`,
    smtp-server.md § Message size limits) refuses locally with exactly a
    `BackendError::Other(..)`, which flows through `ConversationsManager::
    send_new_thread` -> `SendState::failed(e.to_string())` -> the materialized
    thread's `send_state` -> the app's `error-message` — a real, UI-reachable
    trigger for the exact code path the Debug-vs-Display bug lived in
    (composing an over-ceiling message and clicking Send never reaches the
    wire, so no relay/stub-MX setup is exercised; `mail_bridge_mta` is only
    here to let the external recipient resolve, matching the sibling tests
    above).

    Note the failing send is what *closes* the new-thread compose: the manager
    materializes the thread and selects it before calling `send`, so the
    `Failed` state lands on that thread's draft — which is the truth every app
    reads once the new-thread compose is gone.
    """
    app = logged_in_app

    if app.driver.is_windows():
        # This test's own function-level @pytest.mark.web/.tui do NOT narrow it
        # away from the module's broader pytestmark (windows included) — pytest
        # marks are additive, and conftest's --app selection unions module +
        # function markers, so this ran on windows despite the comment above
        # ("so this doesn't widen to every app") never actually holding.
        # Reproduced twice in isolation (2026-08-13): filling
        # dm-text-field via ValuePattern.SetValue with a 2,000,000-character
        # body kills the FlaUI bridge's own HTTP connection (WinError 10054)
        # before Send is even clicked — a bridge/UIA-layer limit with a huge
        # SetValue call, not the product behavior this test actually pins
        # (SmtpBackend's local ceiling refusal). Root cause undiagnosed.
        from helpers.app_surface import skip_unbuilt

        skip_unbuilt(
            app.driver,
            surface="a multi-megabyte compose-body fill via the automation bridge",
            detail=(
                "ValuePattern.SetValue with a 2MB string kills the FlaUI "
                "bridge's HTTP connection (WinError 10054) before Send is "
                "reached; needs a chunked-set fallback or a diagnosed UIA "
                "size limit, not a product fix — windows was never actually "
                "meant to run this regression pin (see comment above)"
            ),
            tracked="",
        )

    recipient = f"recipient@{EXTERNAL_DOMAIN}"
    subject = "Oversized compose"
    # MAX_INLINE_RAW_MESSAGE_BYTES (fauna_mail::transport_limits) is 1,500,000;
    # clear it comfortably — headers + MIME boilerplate add overhead on top of
    # the body bytes.
    body = "a" * 2_000_000

    # body_via_fill: a real user would paste, not type, a chunk this size — and
    # keystroke-simulating 2M characters blows the bridge's per-command timeout.
    app.conversations.start_new_conversation(
        recipient, subject=subject, body=body, body_via_fill=True
    )

    # Convention 14: a POSITIVE wait gets a named generous budget + a deadline
    # poll, so a green run pays only what it actually takes while a loaded box
    # still gets a trustworthy verdict. Sized well above any non-pathological
    # delay — the app has to accept a 2 MB paste, assemble the RFC 5322 message
    # and run the ceiling pre-check, and on web that whole path crosses a
    # CodeMirror document plus a wasm call. The former 20 s was under that on a
    # busy box.
    REFUSAL_SURFACE_BUDGET_S = 120.0
    deadline = time.monotonic() + REFUSAL_SURFACE_BUDGET_S
    error_text = ""
    while time.monotonic() < deadline:
        error_text = app.error_text()
        if error_text:
            break
        time.sleep(0.2)

    assert error_text, (
        f"no error surfaced within {REFUSAL_SURFACE_BUDGET_S:.0f}s after sending an "
        "over-inline-ceiling compose — the refusal was SWALLOWED, which is the "
        "silent-drop shape testing.md point 11 forbids, not a slow render. "
        f"new-conversation-button visible={app.driver.is_visible('new-conversation-button')!r}, "
        f"dm-text-field visible={app.driver.is_visible('dm-text-field')!r}"
    )

    # The ratified render, not the bare backend string: `conversations.md`
    # § Errors & edge cases makes a send failure *always* the single key
    # `conversations.unified.error_send` "with the backend's own rejection as
    # `{message}`". So the localized refusal must arrive inside that template
    # and be the WHOLE of its `{message}` — equality, not containment, because
    # containment cannot see a Rust `Display` that prefixes its own enum-variant
    # name (the `other: ` leak this assertion was red on until 2026-07-30).
    want = S.conversations.unified.error_send(message=S.error.email.too_large)
    assert error_text == want, (
        "an over-inline-ceiling compose must refuse locally and render the "
        f"localized message_too_large text as the whole of error_send's {{message}} — "
        f"got {error_text!r} (want {want!r}). A Debug-formatted rejection would "
        'instead show literal Rust enum syntax, e.g. Other("..."); a Display that '
        'tags its variant would show a "<tag>: " prefix inside the template.'
    )
