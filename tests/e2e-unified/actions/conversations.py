from __future__ import annotations

import re
import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver

_WORD_RE = re.compile(r"[0-9a-z]+")

# The address the inject seam treats as the local user — an injected message's
# default recipient, and the mock mail rail's own address, so reply-all drops it.
# Mirrors `fauna_conversations::manager::TEST_SEAM_SELF_ADDRESS`.
SEAM_SELF_ADDRESS = "me@self-nest.test"


def _word_tokens(text: str) -> list[str]:
    """Lowercased alphanumeric runs of *text* — the comparison unit for matching
    a thread-list ``snippet`` against the body that produced it.

    Words rather than characters because the snippet is the *markdown-stripped*
    body (``ConversationsActions._candidates_matching_body`` explains the
    transform): markers, hrefs and whitespace differ between the two, the words
    do not. A body of pure punctuation (``"..."`` — a real call site) yields no
    tokens, which is the signal that it cannot discriminate anything.
    """
    return _WORD_RE.findall(text.lower())


def _is_subsequence(needle: list[str], haystack: list[str]) -> bool:
    """Whether *needle* appears in *haystack* in order (gaps allowed).

    The markdown strip only ever *removes* words, so the true landing thread's
    snippet tokens are always a subsequence of the injected body's. An empty
    needle is rejected rather than trivially accepted — a snippet with no words
    tells us nothing, and treating it as a match would make every empty-snippet
    thread a candidate for every inject.
    """
    if not needle:
        return False
    it = iter(haystack)
    return all(token in it for token in needle)


class ConversationsActions:
    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    # --- Navigation ---

    def navigate(self) -> None:
        """Navigate to the conversations page."""
        self.driver.navigate_to("conversations")
        self.driver.wait_for("new-conversation-button", timeout=10.0)

    def require_attachment_button_supported(self) -> None:
        """No-op since 2026-07-30 — every app builds `attachment-button` now.

        This carried a tui `skip_unbuilt` gate until tui's leg landed (tui was the
        last of the 7). Kept as a no-op rather than deleted so the call site in
        `test_capability_gating.py` still reads as an explicit statement that the
        surface is a precondition of that test; delete both together if the test is
        ever rewritten. **Do not re-add an app gate here without checking the app's
        `ui-actual` `missing_from_client` first** — the stale gate silently skipped
        the Bluesky capability assertion on tui, which is the failure mode
        convention 7 exists to stop.
        """

    # --- Search / filter ---

    def search_conversations(self, query: str) -> None:
        """Filter the visible thread list by typing ``query`` into
        ``conversation-search-box`` (``conversations.md`` § User actions →
        conversation-search-box "Filter list"). The filter runs in the shared
        manager (``SetSearchQuery`` → ``filter_summaries`` over each thread's label
        + its latest message's full plaintext, deliberately not the bounded
        ``snippet``), so the page re-renders the already-filtered
        ``snapshot().threads``; a
        blank query clears the filter. Canonical home for what the legacy
        ``MessagingActions`` copy provided — ``ConversationsActions`` is the
        unified-model action layer ``test_conversations_search`` drives."""
        self.driver.wait_for("conversation-search-box")
        self.driver.clear_and_type("conversation-search-box", query)

    # --- Compose / send ---

    def start_new_conversation(
        self,
        recipient: str,                  # canonical user@host string for any rail
        subject: str | None = None,      # optional; if set, sends as topic
        body: str = "",
        commit_chip: bool = True,        # commit the recipient chip before send?
        body_via_fill: bool = False,     # paste (fast, atomic) instead of keystroke-simulate?
        files: list[str] | None = None,  # attach through `attachment-button` before send
    ):
        """Click new-conversation-button, fill recipient + (optional) topic + body, send.

        ``files`` attaches each path through ``attachment-button`` (the picker-free
        ``set_input_files`` route every app's e2e seam serves) and waits for its
        ``dm-compose-attachment-chip`` before the next, so Send never races a stage
        that has not landed.

        ``commit_chip=False`` reproduces the real-user flow that surfaced the live
        "I click Send and nothing happens" bug: the user types a recipient and
        clicks Send WITHOUT first pressing Enter / clicking a suggestion to commit
        a chip. The Send action itself must then flush the pending recipient (see
        ``ConversationsManager::send_new_thread``); previously it silently no-op'd.

        ``body_via_fill=True`` drives the body field via ``driver.fill`` (Playwright
        ``fill()`` / the platform paste-equivalent) instead of character-by-character
        ``type_text``. Needed for a body large enough to matter (e.g. exercising the
        inline-ceiling pre-check) — keystroke-simulating megabytes of text is both
        unrealistic (a real user pastes, doesn't type, that much) and too slow for
        the bridge's per-command timeout. Default False preserves exact prior
        behavior for every existing caller.
        """
        # The conversations page is the default landing page after login,
        # but the navigation may not have settled by test time. navigate()
        # is idempotent — it clicks the conversations tab and waits for
        # new-conversation-button to render.
        self.navigate()
        self.driver.click("new-conversation-button")
        self._type_recipient(recipient)
        if commit_chip:
            self.accept_recipient_chip(recipient)
        if subject is not None:
            self.add_topic_to_compose(subject)
        if body_via_fill:
            self.driver.fill("dm-text-field", body)
        else:
            self.driver.type_text("dm-text-field", body)
        if files:
            from helpers.budgets import UI_SETTLE_S
            from helpers.waiting import wait_until

            for staged, path in enumerate(files, start=1):
                self.driver.set_input_files("attachment-button", str(path))
                wait_until(
                    lambda: self.driver.count("dm-compose-attachment-chip") >= staged,
                    UI_SETTLE_S,
                    diagnose=lambda: f"attaching {path} never staged a chip: "
                    f"{self.driver.diagnose('dm-compose-attachment-chip')}",
                )
        self.driver.click("dm-send-button")

    def resolve_recipient(self, recipient: str) -> str:
        """Open the new-conversation composer, type ``recipient``, and wait for
        the recipient picker to reach a terminal resolve state. Returns the
        state string (``"resolved"`` or ``"error"``) read while the composer is
        still open.

        Unlike ``start_new_conversation``, this does NOT accept a chip or send.
        Sending replaces the new-thread composer with the thread view, which
        destroys the ``recipient-resolve-status`` element — so any test that
        asserts on the *resolve status* must read it before a send, while the
        composer is still on screen. Mirrors the inline picker-only flow in
        ``test_rail_locks_after_first_chip`` (which reads picker state without
        sending and passes).
        """
        self.navigate()
        self.driver.click("new-conversation-button")
        self._type_recipient(recipient)
        return self.driver.get_attr("recipient-resolve-status", "state")

    def add_recipient(self, recipient: str) -> None:
        """Address the new-conversation composer that is ALREADY open: type
        ``recipient``, wait for the picker's probe to settle, and commit it as a
        chip — the middle of ``start_new_conversation``, for a journey that
        writes the message before saying who it is for (a draft restored after a
        restart comes back unaddressed if it was saved unaddressed). Opens
        nothing and sends nothing, so the composer and its draft are exactly as
        the caller left them."""
        self._type_recipient(recipient)
        self.accept_recipient_chip(recipient)

    def _type_recipient(self, recipient: str) -> None:
        """Type ``recipient`` into the open composer's picker and wait for a
        terminal resolve state.

        The composer view mounts on a push after the ``+`` click; typing into it
        immediately races that mount under load (two LookupErrors on
        a ``--app macos,ios`` combined run, never on an isolated single-app run
        at the same commit). Convention 14: a bounded ``wait_for``, not a sleep
        or retry-with-scroll (the latter only helps an off-screen element, not
        one that hasn't registered yet)."""
        self.driver.wait_for("recipient-picker-input", timeout=10.0)
        self.driver.type_text("recipient-picker-input", recipient)
        self._wait_resolve()

    def accept_recipient_chip(self, addr: str):
        """Commit the typed text as a recipient chip — click the first
        suggestion when one is visible, otherwise route through a
        TestAgent command that fires the picker's AcceptRequested event
        directly. Pressing Enter via the bridge would be the natural
        path, but Windows 11 sandboxes Win32 SendInput from non-
        foreground processes (the FlaUI bridge isn't foreground), so
        keyboard simulation returns "Access is denied". The test agent
        sidesteps that by calling AcceptCurrentForTest on the visible
        RecipientPicker UserControl on the UI thread.
        """
        if self.driver.count("recipient-picker-suggestion") > 0:
            self.driver.click("recipient-picker-suggestion", index=0)
        else:
            self.driver.call_command("conversations_accept_recipient", {})
        # The accept pushes the chip into the manager and fires the snapshot
        # observer; the chip renders on the *deferred* UI-thread Refresh, which
        # the command return races. An immediate assert won in isolation but
        # flaked under full-suite AT-SPI/UIA contention (reference_windows_e2e_flake)
        # — poll for the chip's appearance instead (condition-based waiting).
        deadline = time.time() + 5.0
        while time.time() < deadline:
            try:
                if self.driver.is_visible("recipient-picker-chip"):
                    return
            except Exception:
                pass
            time.sleep(0.1)
        # Self-diagnosing failure (e2e rule 6): the chip never rendered within
        # the window. visible=False is the expected read here (that is the very
        # thing we polled), so the value of the diagnostic is the count + any
        # surfaced text — count 0 => the accept gesture never pushed a chip into
        # the picker (manager/observer gap); count>0 but invisible => rendered
        # off-screen / on a deferred Refresh the poll outlasted.
        raise AssertionError(
            f"chip not added for {addr}: {self.driver.diagnose('recipient-picker-chip')}; "
            f"resolve_state={self.driver.get_attr('recipient-resolve-status', 'state')!r} — "
            f"an 'error' state here means the picker's discovery probe could not confirm "
            f"{addr!r} (federation.md § Discovery-failure semantics: a known-domain lookup "
            f"failure never silently downgrades to Email)"
        )

    def add_topic_to_compose(self, subject: str):
        """Click topic-toggle-button to reveal subject-input, fill it."""
        self.driver.click("topic-toggle-button")
        # Same click→type race as the composer mount: subject-input
        # is revealed by this click, not present before it — one of the two
        # LookupErrors the row's combined-app run hit was on this exact field.
        self.driver.wait_for("subject-input", timeout=10.0)
        self.driver.type_text("subject-input", subject)

    def cancel_new_conversation(self) -> None:
        """Click the new-thread composer's Cancel/discard affordance
        (``new-conversation-cancel``) — DISCARDS the in-progress new-thread draft
        (the shared ``cancel_new_conversation``) and dismisses the composer
        (``docs/goal/ui/conversations.md`` § Persistence: only an explicit
        cancel/discard or a successful send clears the new-thread draft). The
        canonical element is owned by windows; the other apps add it as their
        legs land."""
        self.driver.click("new-conversation-cancel")

    def step_out_of_new_conversation(self) -> None:
        """Step back out of the new-conversation composer WITHOUT discarding it —
        the everyday way out, which ``docs/goal/ui/conversations.md``
        § Persistence says keeps the half-written message (only
        ``new-conversation-cancel`` or a send clears it). The composer is gone
        from the screen afterwards; its draft is not.

        Per app, as that app's user leaves:

        * **tui** shows the list or a thread, never both, and ui.yaml gives the
          compose sub-page no dismiss element of its own: the conversations tab,
          re-selected, lands on the thread list
          (``apps/fauna-tui/src/app.rs`` → ``conversations::show_list``, which
          keeps the draft — the same path the keymap-only Esc takes).

        * **linux**, **web**, **windows** and **macos** are two-pane: the composer
          is the detail pane beside the list, with no plain back of its own
          (macOS, ``conversations.md`` § Persistence), so the user steps out by
          leaving the page — for the feed, here. The caller comes back with
          :meth:`navigate` before it presses ``new-conversation-button`` again
          (on web this is the route unmount the 2026-08-31 lost-draft bug had).

        * **ios** pushes the composer onto the conversations tab's stack, and the
          phone's back gesture pops it — the iOS test agent's ``nav_back``
          command performs that same pop, whose ``.onDisappear`` calls
          ``deactivate_new_conversation`` (``NewThreadComposeView``).

        * **android** pushes it as the ``conversation_compose`` route, and the
          system back pops it through the screen's ``BackHandler``
          (``deactivateNewConversation()`` then ``popBackStack()``) — the driver
          sends that real back key.

        The other apps raise until their leg of the cross-app lift adds an arm,
        rather than guess.
        """
        if self.driver.is_tui():
            self.navigate()
            return
        if (self.driver.is_linux() or self.driver.is_web() or self.driver.is_windows()
                or self.driver.is_macos()):
            self.driver.navigate_to("feed")
            return
        if self.driver.is_ios():
            self.driver.call_command("nav_back")
            return
        if self.driver.is_android():
            # android pushes the composer as the `conversation_compose` route
            # (`FaunaNavHost.kt`), and the system back is what pops it: the
            # screen's `BackHandler` calls `deactivateNewConversation()` and then
            # `popBackStack()` (`NewThreadComposeScreen.kt`) — the draft-keeping
            # step-out `conversation-drafts.md` § Persistence names. The bridge
            # sends a real back key, so no agent command stands in for it.
            self.driver.press_system_back()
            return
        raise NotImplementedError(
            "step_out_of_new_conversation has no arm for this app until its leg "
            "of the drafts-survive cross-app lift lands"
        )

    def compose_body_text(self) -> str:
        """Return the active composer's current body text (the ``dm-text-field``
        value) — used to assert a draft survived an app restart, or was discarded
        by Cancel (``docs/goal/behavior/file-sync.md`` § Drafts Sync,
        ``docs/goal/ui/conversations.md`` § Persistence). Web + windows + apple
        (macos/ios) + linux today; the remaining native apps add their own
        composer read-back as their draft-persistence legs land — raise loudly
        until then rather than silently passing."""
        if self.driver.is_macos() or self.driver.is_ios():
            # macOS/iOS back `dm-text-field` with an NSTextView/UITextView registered
            # via `.automationField("dm-text-field", text: $body_)` (FaunaKit
            # MarkdownTextEditor / DmComposeBar), so the in-process agent's
            # `/element/text` returns the bound body verbatim — the same
            # registry-direct read linux's in-app agent uses, and "" when empty
            # (no placeholder-scrape fallback). macos verified; ios
            # shares the identical automationField path, exercised as the iOS draft
            # leg re-baselines.
            return self.driver.get_text("dm-text-field")
        if self.driver.is_web():
            # `dm-text-field` on web is a CodeMirror 6 editor, which VIRTUALIZES
            # its viewport: only the on-screen slice of the document exists in the
            # DOM. Reading the element's ``textContent`` (what this arm did until
            # 2026-08-01) therefore silently TRUNCATES any draft longer than a
            # screenful — measured returning 10,812 chars for a 2,000,000-char
            # draft the manager held in full, which would make a draft-persistence
            # assertion (``conversations.md`` § Persistence) pass vacuously on the
            # viewport instead of the draft. Read the editor's own
            # ``state.doc`` through the e2e-gated registry ``MarkdownEditor.svelte``
            # publishes (compiled out of release builds — testing.md point 15).
            # No DOM fallback: a missing registry means the automation surface is
            # absent, and a silent truncating read is exactly what this replaced.
            val = self.driver.eval_js(
                '(() => { const r = window.__fauna_editor_docs;'
                ' if (!r || typeof r["dm-text-field"] !== "function") {'
                '   throw new Error("__fauna_editor_docs[dm-text-field] missing —"'
                '     + " is this a production wasm/SPA flavor? (testing.md point 15)");'
                ' }'
                ' return r["dm-text-field"](); })()'
            )
            return str(val or "")
        if self.driver.is_windows():
            # The windows dm-text-field is a MarkdownRichEditBox exposing the literal
            # markdown source via a custom IValueProvider (ValuePattern.Value =
            # GetPlainText, trailing paragraph CR stripped), so get_text returns the
            # draft body verbatim. The peer reports ControlType Edit, so an EMPTY
            # composer reads back as "" too: the bridge's GetText no longer falls
            # through to scrape the localized placeholder ("Type a message...") for
            # an empty Edit control (it did until 2026-09-21).
            return self.driver.get_text("dm-text-field")
        if self.driver.is_linux():
            # The linux dm-text-field is a gtk::TextView; the automation agent's
            # find.rs:text_of returns its TextBuffer text verbatim (the draft body),
            # and an EMPTY composer reads back as "" (no placeholder fallback) — so a
            # caller checking "empty" can test `draft not in body`.
            return self.driver.get_text("dm-text-field")
        if self.driver.is_tui():
            # tui paints the composer as `Element::input("dm-text-field",
            # compose.body_draft, …)` (`conversations/mod.rs:1276,1762`), and
            # `Element::input`'s `value` IS the element's `text`
            # (`element.rs:828`), so the agent's element read returns the draft
            # body verbatim — the same registry-direct contract as linux, and ""
            # when empty (no placeholder fallback).
            return self.driver.get_text("dm-text-field")
        if self.driver.is_android():
            # android's `dm-text-field` is a Compose text field
            # (`ConversationsComposeBar.kt`), so the bridge's element text is the
            # field's own text — the read `compose_visible_text`'s android arm
            # makes, and `FeedActions.compose_body_text`'s android arm makes for
            # `compose-text-field`. ⚠ That read is the VISIBLE text (Compose
            # semantics carry the VisualTransformation output), so concealed
            # markdown markers are absent from it; the draft journeys type plain
            # text, where source and visible text are the same string.
            return self.driver.get_text("dm-text-field")
        raise NotImplementedError(
            "compose_body_text has no composer read-back for this app until its native draft leg adds one"
        )

    def compose_visible_text(self) -> str:
        """The compose field's VISIBLE text — what the user actually sees after the
        per-editor marker-visibility toggle (``markdown-marker-toggle-button``)
        conceals or reveals the inline markdown markers (``conversations.md`` §
        Compose-field inline markdown styling; the hide-by-default editor
        design is tracked internally).

        On web + android the compose field's plain text read is already the
        VISIBLE/rendered text, so this reads it directly: web's CodeMirror content DOM
        excludes concealed ranges (the hide extension uses ``Decoration.replace``), so
        the content element's ``textContent`` omits hidden markers; android's Compose
        semantics tree reflects the ``VisualTransformation`` output (verified
        empirically, not assumed — see the android branch below).

        linux's plain ``get_text`` (the agent's ``text_of``, ``find.rs``) deliberately
        always reads the buffer with ``include_hidden_chars=true`` — the draft
        round-trip needs the full markdown SOURCE, never the marker-concealed
        display — so it CANNOT answer "what does the user see", same structural
        problem as the two model-backed native clients below. linux therefore
        publishes the rendered text on the same named ``visible`` channel they do
        (``apps/fauna-linux/src/automation/agent.rs``'s ``attr()``: for a
        ``gtk::TextView``, the same buffer read with ``include_hidden_chars=false``,
        honouring the ``md-hidden`` tag ``compose_decoration.rs`` applies over
        concealed markers).

        The two native model-backed clients — windows ``IValueProvider``, apple
        ``automationField`` — return the full SOURCE from their compose read (that read
        backs the draft round-trip), so each publishes the rendered text on a *separate*
        named channel that this reads via ``get_attr(id, "visible")``: windows on
        ``AutomationProperties.HelpText``, apple on the in-process registry's
        ``Entry.visibleText``. Same driver call, same wire attr — the per-app
        difference is only which native slot carries it."""
        if self.driver.is_web():
            val = self.driver.eval_js(
                "(() => { const el = document.querySelector('[data-testid=\"dm-text-field\"]');"
                " return el ? (el.textContent || '') : ''; })()"
            )
            return str(val or "")
        if (
            self.driver.is_linux()
            or self.driver.is_windows()
            or self.driver.is_macos()
            or self.driver.is_ios()
        ):
            # The four model-backed native apps. Their plain compose read is the
            # literal markdown SOURCE and structurally cannot be the rendered text —
            # windows conceals with a Hidden CharacterFormat run that Document.GetText
            # still returns; apple conceals at the glyph layer (zero-width `.null`
            # glyphs), so the NSTextStorage string still holds every marker character;
            # linux's `text_of` (`find.rs`) deliberately reads with
            # `include_hidden_chars=true` for the same reason (the draft round-trip
            # needs the full source). Each therefore publishes the rendered text on
            # the SAME named `visible` attribute, and this is one branch because the
            # driver call is identical — only the native slot behind it differs
            # (windows: AutomationProperties.HelpText, read by the bridge's
            # Actions.cs:GetAttr, which maps any non-name/disabled attr to HelpText;
            # apple: the in-process registry's Entry.visibleText ->
            # MarkdownDecorator.visibleText; linux: `attr()`'s own `"visible"` arm,
            # `apps/fauna-linux/src/automation/agent.rs`, reading the same
            # `gtk::TextView` buffer with `include_hidden_chars=false`). The native
            # twin of web's concealed-excluding textContent.
            return self.driver.get_attr("dm-text-field", "visible") or ""
        if self.driver.is_android():
            # Unlike windows' RichEditBox, Compose's dm-text-field semantics DO reflect
            # the VisualTransformation output, not the raw TextFieldValue — verified via
            # a Robolectric ComposeTestRule probe over a real concealing
            # VisualTransformation (VisualTransformationSemanticsProbeTest), not assumed:
            # the field's accessibility text exposure is the transformed (concealed)
            # string. So a plain get_text already returns the rendered/visible text, same
            # model as web — no separate published channel needed.
            return self.driver.get_text("dm-text-field")
        raise NotImplementedError(
            f"compose_visible_text has no rendered-text read for {type(self.driver).__name__}"
        )

    def add_participant_to_thread(self, addr: str):
        """In an open thread, click the +-add-participant button and pick `addr`."""
        self.driver.click("thread-add-participant-button")
        # The add-participant dialog presents asynchronously: the button click
        # mutates the manager, which fires the snapshot observer, which re-runs
        # the detail view's render() that finally `present()`s the dialog
        # carrying recipient-picker-input. type_text resolves the element
        # immediately (scroll-retry only, no time-based wait), so typing right
        # after the click races the present — a race the inner-loop wins but the
        # full suite loses under AT-SPI contention (full-suite-only red on
        # test_add_to_oneonone). Gate on the input rendering first.
        self.driver.wait_for("recipient-picker-input", timeout=10.0)
        self.driver.type_text("recipient-picker-input", addr)
        self._wait_resolve()
        self.accept_recipient_chip(addr)
        self.driver.click("add-participant-confirm")
        # The confirm click is fire-and-forget on native bridges (AT-SPI
        # `do_action`): it returns before the app's UI thread dispatches the
        # dialog's response handler. That handler runs `confirm_add_participant`,
        # which applies the membership change to the ConversationsManager
        # *synchronously* (the 1:1→group fork or the in-place group add) and
        # then fires the snapshot observer; the deferred observer render closes
        # the dialog once `snapshot.add_participant` is cleared. So the dialog's
        # `recipient-picker-input` leaving the tree is an observable signal that
        # the synchronous mutation has already happened — without it, a caller's
        # immediate `list_threads()` can read pre-mutation state (the in-place
        # add case especially: thread count is unchanged, so only the late
        # participant_count reveals the race). Gate on the dialog closing.
        self._wait_add_participant_dialog_closed()

    def rename_thread(self, new_label: str):
        """Click thread-rename-button, fill new label, confirm via the
        Save button. (Pressing Enter on the field would also work
        through the WinUI KeyDown handler, but Win32 SendInput is
        sandboxed for non-foreground processes on Windows 11; clicking
        the explicit Save button avoids that path.)
        """
        self.driver.click("thread-rename-button")
        # Same click→type race as the others on this row (222): the rename
        # overlay opens on this click, so thread-rename-field isn't present
        # before it.
        self.driver.wait_for("thread-rename-field", timeout=10.0)
        self.driver.clear_and_type("thread-rename-field", new_label)
        self.driver.click("thread-rename-confirm")

    # --- the room policy editor (conversation-rooms.md § Roles and authorization) ---

    def room_class(self) -> str | None:
        """The open thread's class as the header states it — the `class`
        attribute of `thread-room-class` (`end-to-end | community |
        transport-only`), or None when the header carries no class (a rail
        that models no room)."""
        try:
            return self.driver.get_attr("thread-room-class", "class")
        except Exception:
            return None

    def prospective_room_class(self) -> str | None:
        """The new-thread composer's class for the room its committed chips
        would create — the `class` attribute of `recipient-picker-class`
        (`end-to-end | community | transport-only`), or None while no chip is
        committed. Derived in shared Rust (`prospective_room_class`) from each
        chip's rail, so it is the one cross-app read of WHICH rail claimed a
        committed address: a Fauna chip reads `end-to-end`, a bridged (email)
        chip `transport-only` — where `recipient-resolve-status`'s `resolved`
        says only that SOME rail did."""
        try:
            return self.driver.get_attr("recipient-picker-class", "class") or None
        except Exception:
            return None

    def send_in_open_thread(self, body: str) -> None:
        """Type ``body`` into the open thread's `dm-text-field` and click
        `dm-send-button` — a user's send, through the compose bar."""
        self.driver.wait_for("dm-text-field", timeout=10.0)
        self.driver.type_text("dm-text-field", body)
        self.driver.click("dm-send-button")

    def home_nest_included(self) -> bool:
        """Whether the open new-thread composer seats the home nest — the
        `checked` attribute of `recipient-picker-home-nest-toggle`."""
        return self.driver.get_attr("recipient-picker-home-nest-toggle", "checked") == "true"

    def toggle_home_nest(self) -> None:
        """Flip `recipient-picker-home-nest-toggle` on the open composer. ON
        makes the first send FOUND a community room (the `room.create`
        ceremony) rather than bootstrap an end-to-end group; the
        `recipient-picker-class` statement follows it."""
        self.driver.click("recipient-picker-home-nest-toggle")

    def room_invitation_count(self) -> int:
        """How many `room-invitation[i]` rows stand atop the list (0 while
        none do — the section is absent, not empty)."""
        self.navigate()
        return self.driver.count("room-invitation")

    def room_invitation_text(self, index: int = 0) -> str:
        return self.driver.get_text("room-invitation", index=index)

    def accept_room_invitation(self, index: int = 0) -> None:
        """Click `room-invitation-accept-button[index]`. On success the room
        opens as a thread (the page follows the manager into the detail
        view); a refusal lands on `error-message`."""
        self.navigate()
        self.driver.click("room-invitation-accept-button", index=index)

    def decline_room_invitation(self, index: int = 0) -> None:
        self.navigate()
        self.driver.click("room-invitation-decline-button", index=index)

    def room_nest_read_staged(self) -> bool | None:
        """The open editor's staged home-nest read — `room-nest-read-toggle`'s
        `checked` attribute — or None when the control is not painted (an
        end-to-end room, or a community room whose answer this device has not
        read yet)."""
        if not self.driver.is_visible("room-nest-read-toggle"):
            return None
        return self.driver.get_attr("room-nest-read-toggle", "checked") == "true"

    def toggle_room_nest_read(self) -> None:
        """Flip the staged home-nest read; `save_room_settings` commits it."""
        self.driver.click("room-nest-read-toggle")

    def pending_invite_count(self) -> int:
        """How many `room-pending-invite[i]` rows the open editor paints — 0
        while the section is absent: nothing pending, or a list the home nest
        has not served this device yet (`ThreadDetail.room.pending_invites`
        is `None` until the floor's own cadence reads it). The nest serves a
        viewer exactly the invitations that viewer may withdraw
        (`conversation-rooms.md` § Join rules and invites → *Pending
        invitations are visible to whoever may withdraw them*)."""
        return self.driver.count("room-pending-invite")

    def pending_invite_text(self, index: int = 0) -> str:
        """The row's sentence — the invitee, who invited them, the rank on
        offer (shared wording, `RoomPendingInviteSnapshot::text`)."""
        return self.driver.get_text("room-pending-invite", index=index)

    def pending_invite_lapsed(self, index: int = 0) -> bool:
        """The row's `lapsed` attribute: `True` when the accept door would
        refuse it today (its inviter was demoted, departed or succeeded), so
        it is listed only to be cleared."""
        return self.driver.get_attr("room-pending-invite", "lapsed", index=index) == "true"

    def withdraw_pending_invite(self, index: int = 0) -> None:
        """Click `room-pending-invite-withdraw-button[index]`. Acts at once,
        outside Save's staged set like `room-leave-button`, and needs no
        confirm (inviting again undoes it). On success the list is read again
        and the row leaves the editor; a refusal lands on `error-message`
        (`conversations.unified.error_withdraw_room_invite`). The invitee is
        told nothing — their envelope simply stops standing."""
        self.driver.click("room-pending-invite-withdraw-button", index=index)
    def room_labeler_index(self, labeler_hex: str) -> int | None:
        """The row index of ``labeler_hex`` among the open editor's
        `room-labeler-toggle[i]` rows, or None when it is not listed. Found by
        id rather than position: the catalog is nest-global, so a shared
        session nest may list other runs' labelers around this one. A row's
        text carries the id's first 16 hex characters."""
        prefix = labeler_hex.lower()[:16]
        for i in range(self.driver.count("room-labeler-toggle")):
            if prefix in self.driver.get_text("room-labeler-toggle", index=i):
                return i
        return None

    def room_labeler_staged(self, index: int) -> bool:
        """`room-labeler-toggle[index]`'s `checked` attribute — whether the
        open editor's staged set names that labeler."""
        return self.driver.get_attr("room-labeler-toggle", "checked", index=index) == "true"

    def toggle_room_labeler(self, index: int) -> None:
        """Name or un-name the labeler on row `index`; `save_room_settings`
        commits the whole set. Greyed unless owner or admin, and an unnamed
        row greyed once four are named."""
        self.driver.click("room-labeler-toggle", index=index)

    def inspect_room_labeler(self, index: int) -> str:
        """Open row `index`'s catalog inspect view in place and return its
        `labeler-inspect-metadata` text (the rendered signed metadata)."""
        self.driver.click("room-labeler-inspect-button", index=index)
        self.driver.wait_for("labeler-inspect-panel", timeout=10.0)
        return self.driver.get_text("labeler-inspect-metadata")

    def close_labeler_inspect(self) -> None:
        """`labeler-inspect-close-button` — back to the room editor."""
        self.driver.click("labeler-inspect-close-button")

    def open_room_settings(self):
        """Click `thread-room-settings-button` and wait for the editor
        (`room_settings` sub-page) to present. The door is painted AND LIVE
        whenever the thread is a room (`ui/conversations.md` § Element IDs,
        widened 2026-09-20): leaving is a plain member's verb and lives inside
        the editor, so every member opens it. The greying is per control
        INSIDE — the selects and Save off `capabilities.can_set_policy`, the
        walk-out off `can_leave_room` — so a member's editor opens read-only
        with only `room-leave-button` live."""
        self.driver.click("thread-room-settings-button")
        self.driver.wait_for("room-settings-save-button", timeout=10.0)

    def set_room_join_rule(self, rule: str) -> None:
        """Pick `rule` — `invite` (owner and admins) or `member-invite` (any
        member) — on `room-join-rule-select`. Tokens, not labels: the
        cross-app `select(id, token)` contract, like `event-detail-reminder-select`."""
        self.driver.select("room-join-rule-select", rule)

    def set_room_history_policy(self, policy: str) -> None:
        """Pick `policy` — `none` or `full` — on `room-history-policy-select`."""
        self.driver.select("room-history-policy-select", policy)

    def toggle_room_admin(self, index: int) -> None:
        """Flip the admin flag staged for the participant at `index` —
        `room-admin-toggle[index]`, indexed like `thread-member-chip[i]`
        (pick `index` off `ThreadSummary.participant_actor_ids`). Greyed
        unless `capabilities.can_appoint_admins` (owner only)."""
        self.driver.click("room-admin-toggle", index=index)

    def room_admin_staged(self, index: int) -> bool:
        """Whether the editor currently stages the participant at `index` as
        an admin (the toggle's `checked` attribute)."""
        return self.driver.get_attr("room-admin-toggle", "checked", index=index) == "true"

    def toggle_room_owner_transfer(self, index: int) -> None:
        """Stage the participant at `index` as the room's new owner — or
        un-stage it — on `room-owner-transfer-button[index]`, indexed like
        `thread-member-chip[i]`. At most one row is staged at a time. Greyed
        unless `capabilities.can_transfer_ownership` (owner only). Save posts
        the hand-over LAST, after every other staged change
        (`conversation-rooms.md` § Roles and authorization → Ownership
        transfer); the roles flip on every seat only once the new owner's
        device has committed the countersigned policy."""
        self.driver.click("room-owner-transfer-button", index=index)

    def room_owner_transfer_staged(self, index: int) -> bool:
        """Whether the editor currently stages the participant at `index` as
        the new owner (the control's `checked` attribute)."""
        return (
            self.driver.get_attr("room-owner-transfer-button", "checked", index=index) == "true"
        )

    def room_leave_enabled(self) -> bool:
        """Whether the open editor's `room-leave-button` is live.

        Greyed, never hidden, off `capabilities.can_leave_room` — which the
        roles table closes for the **owner**, who hands the room over first
        (`conversation-rooms.md` § Roles and authorization → *Leaving — the
        mechanism*). So a `False` here on an owner's seat is the affordance
        working, not a missing control.
        """
        return self.driver.is_enabled("room-leave-button")

    def leave_room(self, timeout_s: float = 30.0) -> None:
        """Walk out of the open room: `room-leave-button` → `room-leave-confirm`.

        Deliberately NOT routed through `save_room_settings`: leaving is an
        immediate act rather than a staged policy edit, so the confirm acts on
        its own and the editor closes with it. What happens beneath is the
        rail's business, never this helper's — every class opens the
        self-scoped `room.leave` door.

        Waits for the editor to close, which is the observable for "the
        departure was taken". A refusal keeps it open with the page's
        `error-message`, so read `error_text()` before asserting on a roster
        when it does not close.
        """
        self.driver.click("room-leave-button")
        self.driver.wait_for("room-leave-confirm", timeout=10.0)
        self.driver.click("room-leave-confirm")
        deadline = time.time() + timeout_s
        while time.time() < deadline:
            if not self.driver.is_visible("room-leave-confirm"):
                return
            time.sleep(0.25)
        raise AssertionError(
            f"the room editor did not close within {timeout_s}s after confirming "
            f"the departure — the leave may have been refused (the owner's "
            f"transfer-first refusal, or a report the floor did not take); read "
            f"error-message: "
            f"{self.driver.diagnose('room-leave-confirm')} "
            f"{self.driver.diagnose('error-message')}"
        )

    def save_room_settings(self, timeout_s: float = 30.0) -> None:
        """Click `room-settings-save-button` and wait for the editor to close.

        The save commits every staged change as its own policy commit on the
        room's MLS channel (join rule, history policy, each appointment /
        demotion), awaited on the agent's click path; the editor closes only
        when all of them landed, and stays open — the page's `error-message`
        carrying the refusal — when one did not. So "closed" is the observable
        for "every change is in the agreed group context"; read `error_text()`
        before asserting on the projection when it does not close.
        """
        self.driver.click("room-settings-save-button")
        deadline = time.time() + timeout_s
        while time.time() < deadline:
            if not self.driver.is_visible("room-settings-save-button"):
                return
            time.sleep(0.25)
        raise AssertionError(
            f"room-settings editor did not close within {timeout_s}s — a "
            f"staged change may have been refused (check error-message): "
            f"{self.driver.diagnose('room-settings-save-button')} "
            f"{self.driver.diagnose('error-message')}"
        )

    def read_subject_dividers(self) -> list[str]:
        """Return text of all subject-divider elements in order."""
        n = self.driver.count("subject-divider")
        return [self.driver.get_text("subject-divider", index=i) for i in range(n)]

    # --- Bridge-routed commands and state reads ---

    def list_threads(self) -> list:
        """Return thread summaries (list of objects with .label, .flavor,
        .snippet, .message_count, .participant_count, .thread_id, .rail,
        .unread_count). Reads ``state.data.conversation_threads`` pushed
        by the running app's ``AppDataSnapshot.GetConversationsThreadsForState``.

        Forces a fresh state push by issuing an empty ``patch`` first.
        Bridge clients only push state at the end of command processing
        and on idle polls (~1s cadence), not on every UIA click. Without
        this settle, callers that read state immediately after a UI
        action that mutates the manager (e.g. the add-participant
        confirm click) get a cached pre-mutation snapshot.
        """
        try:
            self.driver.set_state({})
        except Exception:
            pass
        rows = self.driver.get_state("data.conversation_threads") or []
        return [_ThreadSummary(r) for r in rows]

    def inject_inbound_for_test(
        self,
        rail: str,
        sender: str,
        subject: str | None = None,
        body: str = "",
        in_reply_to=None,
        force_subject_change: str | None = None,
        attachments: list[dict] | None = None,
        is_own: bool = False,
        labels: list[dict] | None = None,
        recipients: list[str] | None = None,
        bridge_id: str | None = None,
    ) -> str:
        """Inject an inbound message via the test-helper UniFFI route.

        Posts a ``conversations_inject_inbound`` command to the bridge; the
        running app routes it through ``ConversationsManager.inject_inbound_for_test``
        which fires the snapshot observer and updates the page. Returns the
        message id assigned to the injected message (matches what later
        ``in_reply_to=`` references can use).

        ``attachments`` is an optional list of
        ``{"filename", "mime_type", "data_base64"}`` dicts (the unified shape
        every app parses for ``docs/goal/ui/conversations.md`` § Attachments).
        Each app decodes the bytes and routes them through the shared Rust seam
        ``ConversationsManager.make_attachment_for_test``, which caches them under
        their content-addressed ``blob_hash`` (BLAKE3) and derives ``is_image``
        from the MIME type, so the rendered bubble resolves ``dm-attachment-image``
        / ``dm-attachment-file`` to the real bytes — exactly as a real inbound MIME
        parse would. Clients that haven't lifted the inject handler yet ignore the
        field (build an empty attachment list), so the assertion just fails on them
        until they add it — the same staged pattern as the markdown lift.

        ``is_own=True`` forces the injected message's ``is_own`` flag (the manager
        ``inject_own_for_test`` seam) so a test can materialise an OWN bubble —
        needed for the sender-only delete affordance + own-reaction pill — without
        a live send (a client wiring the real FaunaMls backend at login, e.g.
        linux, can't send into an injected/groupless thread). Prefer
        ``seed_own_message`` over calling this directly — it also opens the
        thread it seeded. Clients that haven't lifted the ``is_own`` branch
        inject as a normal inbound (so the delete affordance is absent and the
        test fails on them until they add it — same staged pattern as
        ``attachments``).

        ``labels`` is an optional list of ``{"category", "confidence_per_mille"}``
        dicts stamped directly onto the resulting message's ``labels`` (the
        manager ``inject_inbound_with_labels_for_test`` seam). The generic inject
        path does not classify (only the real MLS receive path's
        ``observe_local_detection`` does), so a test needing a *labeled* bubble —
        the family-safety content-floor render, the content-label badge — stages
        the labels here, exactly as the feed stages ``TestPostSpec.labels``.
        Clients that haven't lifted the ``labels`` branch inject as a normal
        (unlabeled) inbound (same staged pattern as ``attachments`` / ``is_own``).

        ``recipients`` names the message's whole To/Cc set, so a mail thread can
        hold more people than its sender and the seam's self address
        (:data:`SEAM_SELF_ADDRESS`, which the mock mail rail treats as the user,
        so reply-all drops it). Omitted, the message goes to the self address
        alone. The shared parser ``inject_inbound_from_test_payload`` reads it
        (tui, linux); an app with its own payload parse ignores the key until it
        lifts it, and a test relying on it then fails there, which is the point.

        ``bridge_id`` names the bridge a ``rail="Bridged"`` message rides — a
        bridged address is the bridge's manifest id *and* the far network's
        spelling (``docs/goal/ui/conversations.md`` § Where logic lives → *The
        ``Bridged`` adapter*, ruling 2 (c)), so the shared parser refuses a
        Bridged inject without one. Every app hands the payload whole to that
        parser, so the key reaches all seven at once.
        """
        import uuid
        msg_id = f"msg-{uuid.uuid4().hex[:12]}"
        # ``force_subject_change`` is passed as a separate field so the
        # bridge command keeps using ``subject`` for thread keying and
        # routes the override straight onto the resulting message's
        # ``subject_line`` (drives subject-divider rendering inside an
        # already-keyed thread).
        payload = {
            "rail": rail,
            "sender": sender,
            "recipient": SEAM_SELF_ADDRESS,
            "subject": subject,
            "body": body,
            "message_id": msg_id,
            "in_reply_to": in_reply_to,
            "force_subject_change": force_subject_change,
            "attachments": attachments or [],
            "is_own": is_own,
            "labels": labels or [],
        }
        if recipients is not None:
            payload["recipients"] = list(recipients)
        if bridge_id is not None:
            payload["bridge_id"] = bridge_id
        self.driver.call_command("conversations_inject_inbound", payload)
        return msg_id

    def seed_resolved_link_preview(
        self,
        url: str,
        title: str = "",
        description: str = "",
        image_hash: str | None = None,
    ) -> None:
        """Seed a PRE-RESOLVED link-preview (render-model.md § D4) for ``url`` so an
        injected bubble whose body is the standalone ``[url](url)`` markdown paragraph
        folds its ``LinkPreview`` block ``Resolved`` — the bubble then paints the
        ``link-preview-card`` (title / description / domain), with the og:image gated
        behind the message's ``load-remote-content-button`` reveal.

        The conversations twin of the feed's ``link_preview`` inject spec
        (``test_feed_link_preview.py``): the resolve is **mocked** (tier_2) — a real
        resolve needs a live nest OpenGraph fetch (SSRF-guarded, render-model.md § D4),
        so the ``Resolved`` card is seeded deterministically, exactly as the feed test
        seeds its ``feed_inject_posts`` ``link_preview`` spec. The driver routes the
        unified ``conversations_seed_resolved_link_preview`` command per platform (web
        wasm ``seedResolvedLinkPreviewForTest`` / linux bridge handler). ``image_hash``
        is the og:image's content-addressed blob hash; ``None`` paints no image child.
        """
        self.driver.call_command("conversations_seed_resolved_link_preview", {
            "url": url,
            "title": title,
            "description": description,
            "image_hash": image_hash,
        })

    def seed_own_message(self, body: str, recipient: str = "bob@self-nest.test") -> str:
        """Materialise an OWN message bubble (``is_own=True``) in an open FaunaMls
        thread, for the sender-only delete + own-reaction render tests. Returns the
        **thread id** it seeded, so the caller can address that thread by identity
        rather than needling the peer out of the list.

        ⚠ Pass your own ``recipient`` if the test asserts on bubble/pill COUNTS: the
        peer is the thread key, and the default is shared, so two tests taking it
        would seed one thread and see each other's messages (threads accumulate —
        ``nest_instance``/``test_user`` are session-scoped).

        One call for every app (2026-08-17) — the ``inject_own_for_test``
        seam (manager forces ``is_own`` on a keyed inbound), then open the thread
        it keyed. Was per-app branched (linux/tui/macos/ios/windows on the seam,
        web on ``create_mls_group`` + a compose-send) until web's agent landed
        ``is_own`` 2026-08-12 (the residual — the wasm face
        ``injectOwnForTest`` + the ``conversations_inject_inbound`` handler arm,
        ``apps/fauna-web/src/lib/conversations.ts``); that was the last app whose
        agent ignored the flag. **Uniformity, priority #1** — one behavior, seven
        apps, one code path.

        Leaves an own bubble in the open thread; polls for it to render before
        returning.

        ⚠ **NOT an MLS witness — this seam creates NO crypto state.** It is a
        thread-store fixture over ``MockRailBackend``
        (``libs/fauna-conversations/src/manager.rs``): it materialises a rendered
        thread and touches the MLS *engine* not at all, so the openMLS
        ``provider`` gains no channel and nothing lands in the ``__mls`` replica
        at rest. A test whose precondition is *"real MLS state exists"* — a
        replica re-seal, a cross-device restore, a succession's leg 3 — is
        **vacuous or red** when seeded here, and the red looks exactly like a
        broken product leg. Measured 2026-08-31: the succession journey
        ``test_the_successors_conversations_unlock_without_a_user_command`` spent
        four investigation passes on a leg that had been green since 2026-08-21,
        because an instrumented run finally reported ``provider_ours=true
        channels=0 histories=0 owed=0`` — the re-seal loop never ran an
        iteration. Use ``enable_real_faunamls()`` + ``real_resolve_send_new()``
        instead, and gate on ``helpers.conversations_restart.wait_replica_settled``
        so the debounced autosave has actually flushed.
        """
        thread_id = self.inject_and_open_thread(
            rail="FaunaMls", sender=recipient, body=body, is_own=True
        )
        deadline = time.time() + 8.0
        while time.time() < deadline:
            try:
                if self.driver.count("dm-message-text") >= 1:
                    return thread_id
            except Exception:
                pass
            time.sleep(0.2)
        raise AssertionError(
            "own message bubble did not render after seed_own_message; "
            f"{self.driver.diagnose('dm-message-text')}. Check the app's "
            "`error-message` — the test agent surfaces a refused inject instead "
            "of swallowing it."
        )

    def selected_message_index(self) -> int | None:
        """Index of the message the open thread has *selected*, or None.

        The cross-app observable for `SearchNav::Mail`'s second half is a
        ``selected`` attribute on ``dm-message-timestamp`` (``"true"``/
        ``"false"``), not a separate element — ui.yaml `dm-message-bubble`
        explains why (the timestamp is the one bubble child painted by every
        render arm, so a hit on a deleted / muted / content-collapsed message is
        still markable). See `conversations.md` § The selected message.

        Fails loudly if more than one bubble claims to be selected: "the
        selected message" is singular by definition, and a marker that spreads
        would otherwise read as a pass on any assertion that only checks the
        expected index.
        """
        selected = [
            i
            for i, marked in enumerate(
                self.driver.get_attrs("dm-message-timestamp", "selected")
            )
            if marked == "true"
        ]
        assert len(selected) <= 1, (
            f"exactly one message may be selected, found {len(selected)} at {selected}"
        )
        return selected[0] if selected else None

    def message_in_view(self, body: str) -> bool:
        """Whether the bubble whose ``dm-message-text`` contains ``body`` is in
        the open thread's visible band — the "bring it into view" half of the
        selected-message contract (`conversations.md` § The selected message).

        A bubble the driver cannot list at all is not in view (a lazy list
        has not realized it). A listed one is asked through
        ``driver.in_viewport``, which raises on an app that cannot answer
        rather than reading every row as in view.
        """
        for i, text in enumerate(self.driver.get_texts("dm-message-text")):
            if body in (text or ""):
                return self.driver.in_viewport("dm-message-text", index=i)
        return False

    def inject_send_failure_for_test(self, thread_id: str, reason: str) -> None:
        """Stamp ``thread_id``'s compose into ``send_state = Failed { reason }``
        and select it — the same observable state ``send_new_thread`` leaves
        after a backend send error — so a test can assert the page-level
        ``error-message`` surface renders the reason.

        Why a seam and not a real failure: a mail-OFF nest does NOT fail the
        send — ``email_handlers::enqueue_outbound`` writes a queue row and
        returns ``Ok``, so the client send succeeds into a void and never
        reaches ``Failed``. There is no product path that fails a send on
        demand, so the deterministic-failure test drives this injection seam
        (``ConversationsManager::inject_send_failure_for_test``) instead of a
        fail-on-demand backend.

        ``reason`` stays a plain string — unlike
        :meth:`inject_page_error_for_test` this seam takes no ``key``, because
        sends have exactly one producer and therefore one key: shared Rust's
        ``SendState::failed`` wraps the detail into
        ``conversations.unified.error_send`` + ``{message}``. So the app renders
        the *resolved template* containing ``reason``, not ``reason`` verbatim —
        assert containment, and that the raw key never reaches the surface.
        """
        self.driver.call_command("conversations_inject_send_failure", {
            "thread_id": thread_id,
            "reason": reason,
        })

    def select_message(self, thread_id: str, message_id: str) -> None:
        """Drive ``ConversationsManager.select_thread_and_message`` directly via
        the ``conversations_select_message`` command — apple (macOS + iOS),
        linux, tui and windows.

        This is the same call `SearchNav::Mail`'s real Search-result tap makes
        (``ConversationsVM.selectThreadAndMessage`` on apple), but reached
        without a real Search hit: iOS's local content index is query-only
        (``CLIENT_BUILDS_INDEX`` is false there,
        ``libs/fauna-ffi/src/index_launch.rs``), so a single-seat run never
        builds a segment to search and `SearchNav::Mail` can never surface on a
        phone regardless of the shared paint. This seam isolates the downstream
        half of the contract — does calling ``select_thread_and_message`` paint
        the right bubble and scroll it into view — so it is provable on a real
        iOS simulator without a two-device sync run
        (``docs/goal/ui/conversations.md`` § The selected message). Use
        :meth:`selected_message_index` to read the result.
        """
        self.driver.call_command("conversations_select_message", {
            "thread_id": thread_id,
            "message_id": message_id,
        })

    def inject_page_error_for_test(
        self,
        message: str,
        key: str = "conversations.unified.error_add_participant",
    ) -> None:
        """Stamp ``ConversationsSnapshot.error`` — the observable state a failed
        **membership/label** wire op leaves (``confirm_add_participant`` /
        ``remove_participant`` / ``rename_thread``) — so a test can assert the
        page-level ``error-message`` surface renders it.

        The membership twin of :meth:`inject_send_failure_for_test`, and a seam
        for the same stated reason: no *product* path fails one of those ops on
        demand (a real failure needs a nest that rejects the Commit, or a
        cross-nest member whose roster the owner's nest cannot read — see
        ``docs/goal/architecture/mls-group-key-material.md`` § M2 *Admitting a
        member*), so without an injection the per-app surface is unassertable.

        ``key`` is an i18n key and ``message`` its ``{message}`` substitution:
        the app resolves them through its own localization pipeline, so a test
        can assert on the *resolved* text and catch a client that paints the raw
        key.
        """
        self.driver.call_command("conversations_inject_page_error", {
            "key": key,
            "message": message,
        })

    def inject_and_resolve_thread(
        self,
        rail: str,
        sender: str,
        subject: str | None = None,
        body: str = "",
        timeout_s: float = 10.0,
        **kwargs,
    ) -> str:
        """Inject an inbound and return the **id of the thread it landed in**,
        without opening it. The identity-resolving replacement for injecting and
        then re-finding the thread with a fuzzy needle. Use
        ``inject_and_open_thread`` unless you need to do something (seed a link
        preview, inject again) between the inject and the open.

        Why this exists. ``nest_instance``/``test_user`` are **session-scoped**
        and the app process is reused, so threads **accumulate across the whole
        run**. A needle (a sender substring, a subject) is therefore NOT a stable
        handle on a thread: an earlier test's rename replaces the label, and a
        later inbound from the same participant merges in and replaces the
        snippet — so the needle stops matching the very thread it created, and
        the lookup either dies (``No thread found containing sender ...``) or,
        worse, silently opens somebody else's thread. That made the conversations
        suite **order-dependent**: a test's outcome became a function of which
        tests ran before it. Resolving by the app-assigned ``thread_id`` is immune
        to both, and it is the same lesson as the folder per-row-index trap:
        *resolve by identity, never by a fuzzy first match over shared mutable
        state* (``docs/goal/architecture/testing.md`` § Cross-app e2e
        conventions).

        The thread id is derived with no app-side change (so it works on all 6
        apps today): snapshot the thread list, inject, then poll for the one
        thread that is **new**, or whose ``message_count`` **grew**, or whose
        ``snippet`` (the latest message) **changed**. That covers both keying
        outcomes — a fresh thread, or a merge into an existing participant/subject
        thread — off fields that already ride the shared state snapshot. Two merge
        signals rather than one because ``message_count`` is asserted on linux /
        web / macos but is not proven to be maintained by every app's snapshot;
        the snippet moves whenever a message lands, so a client that tracks only
        one of the two still resolves.

        **Other threads change while we wait, and that is normal.** The change
        signals above detect *that* something moved, not *what* moved it, so on
        their own they assume nothing else in the list mutates during the
        resolve — global quiescence, which no shared box provides: the
        conversations receive loop drains inbound every 2 s under e2e, and on web
        a thread's detail-derived ``message_count`` hydrates lazily, so prior rows
        can "grow" long after they were seeded. That assumption failed in both
        directions — loudly when two threads changed (*"one inject landed in
        several threads at once"*, a hard failure on a perfectly normal
        condition) and **silently when one unrelated thread changed before ours
        did**, which returned somebody else's thread.

        So every candidate set, of any size, is narrowed by the injected ``body``
        — already known, and by construction the *last* message of the thread it
        landed in (:meth:`_candidates_matching_body`). A body matching exactly one
        candidate resolves; matching several is genuine ambiguity and still fails
        loudly, because silently picking one is the wrong-thread read this helper
        exists to prevent; matching none means our message has not surfaced *yet*,
        so the poll simply continues. Only if the whole budget passes with no
        match does a lone changed thread win by default — that is the answer this
        helper gave before content matching existed, so the narrowing can never
        resolve *less* often than it used to, and it is also the right answer for
        the one case content matching cannot see: a second arrival landing in our
        own thread and taking over its preview.
        """
        before = {
            t.thread_id: (t.message_count, t.snippet) for t in self.list_threads()
        }
        self.inject_inbound_for_test(
            rail=rail, sender=sender, subject=subject, body=body, **kwargs
        )

        deadline = time.time() + timeout_s
        unmatched: list = []
        while time.time() < deadline:
            landed = [
                t
                for t in self.list_threads()
                if t.thread_id not in before
                or t.message_count > before[t.thread_id][0]
                or t.snippet != before[t.thread_id][1]
            ]
            if landed:
                matching = self._candidates_matching_body(landed, body)
                if matching is None:
                    # The body carries no word to discriminate with, so the raw
                    # change signals are all there is: exactly today's behavior.
                    if len(landed) == 1:
                        return landed[0].thread_id
                    raise AssertionError(
                        "one inject landed in several threads at once "
                        f"({[t.thread_id for t in landed]}) and body={body!r} "
                        "carries no word to disambiguate them — the thread list "
                        "is mutating concurrently and identity cannot be resolved"
                    )
                if len(matching) == 1:
                    return matching[0].thread_id
                if len(matching) > 1:
                    raise AssertionError(
                        f"the injected body {body!r} ends several changed threads "
                        f"at once ({[t.thread_id for t in matching]}), so identity "
                        "cannot be resolved. Inject a body unique to this test."
                    )
                # Zero matches: every changed thread moved for some *other*
                # reason and ours has not surfaced yet. Keep polling — this is
                # the case that used to fail on a perfectly normal condition,
                # and, when only one thread had moved, used to silently return
                # that unrelated thread instead.
                unmatched = landed
            time.sleep(0.2)

        if len(unmatched) == 1:
            # One thread moved, nothing else did, and our body is not its
            # snippet. Overwhelmingly this is our own thread with a later
            # arrival already sitting on top of the preview, so the raw change
            # signal is the better answer — and it is exactly the answer this
            # helper gave before content matching existed, which is what keeps
            # the narrowing from ever resolving *less* often than it used to.
            return unmatched[0].thread_id

        threads_now = self.list_threads()
        detail = ""
        if unmatched:
            detail = (
                f" {len(unmatched)} threads did change while waiting "
                f"({[(t.thread_id, t.snippet) for t in unmatched]}), but none "
                f"ends on the injected body — so the inject never landed, rather "
                f"than landing somewhere ambiguous."
            )
        raise AssertionError(
            f"injected message (rail={rail!r} sender={sender!r} "
            f"subject={subject!r} body={body!r}) never surfaced in any thread "
            f"within {timeout_s}s — no thread was created and none grew a "
            f"message.{detail} Threads now: "
            f"{[(t.thread_id, t.label, t.message_count) for t in threads_now]}"
        )

    @staticmethod
    def _candidates_matching_body(candidates: list, body: str) -> list | None:
        """Narrow *candidates* to the thread(s) whose ``snippet`` is the injected
        ``body``. Returns ``None`` when the body carries no usable signal, which
        the caller must treat as "cannot discriminate" rather than "no match".

        The comparison models the transforms the snippet actually goes through:
        ``ThreadSummary.snippet`` is ``markdown_to_plaintext`` over a byte-bounded
        prefix of the last message's body, capped back to a word boundary
        (``libs/fauna-conversations/src/store/threads.rs::snippet_preview``), and
        every one of the 7 apps serializes that same shared field verbatim into
        ``data.conversation_threads`` — linux/tui via ``state_json.rs``, windows
        via ``AppDataSnapshot``, apple/android/web via their FFI mirrors — so
        there is no per-app truncation to model: only the markdown strip and that
        one shared prefix cut, which drops trailing words and never splits one.

        That strip drops markers and hrefs and collapses whitespace: it can only
        *remove* words, never add or reorder them. So comparing word tokens
        (lowercased alphanumeric runs) makes the snippet's tokens a **subsequence**
        of the body's — a condition the true landing thread always satisfies,
        which is what keeps this from ever resolving *less* often than the raw
        change signals did. Exact token equality is checked first so a body that
        is a strict prefix of another cannot swallow it; the subsequence rule is
        the fallback for bodies whose preview legitimately drops words (a
        markdown link contributes its label, not its href).
        """
        body_tokens = _word_tokens(body)
        if not body_tokens:
            return None
        exact = [t for t in candidates if _word_tokens(t.snippet) == body_tokens]
        if len(exact) == 1:
            return exact
        return [
            t for t in candidates if _is_subsequence(_word_tokens(t.snippet), body_tokens)
        ]

    def inject_and_open_thread(
        self,
        rail: str,
        sender: str,
        subject: str | None = None,
        body: str = "",
        timeout_s: float = 10.0,
        **kwargs,
    ) -> str:
        """Inject an inbound and open the thread it landed in, resolved by
        **identity**. Returns the thread id.

        Prefer this over ``inject_inbound_for_test`` + ``open_thread_for_sender``
        / ``open_thread_with_subject`` for any test that opens the thread it just
        injected (which is nearly all of them) — see
        ``inject_and_resolve_thread`` for why a needle is not a handle.
        """
        thread_id = self.inject_and_resolve_thread(
            rail=rail,
            sender=sender,
            subject=subject,
            body=body,
            timeout_s=timeout_s,
            **kwargs,
        )
        self.open_thread_by_id(thread_id)
        return thread_id

    def open_thread_by_id(self, thread_id: str, *, timeout_s: float = 15.0):
        """Open the thread whose app-assigned ``thread_id`` matches — the
        unambiguous pick, and the one to reach for whenever the caller already
        knows the identity of the thread it wants (e.g. it just injected into it,
        via ``inject_and_open_thread``).

        The identity twin of ``open_thread_by_channel``; unlike the needle-based
        openers below it cannot be defeated by a rename, a merge, or a colliding
        needle from another test sharing the session-scoped nest.

        ``list_threads()`` force-pushes a fresh *state* read, which can outrun
        the UI's own render/registration pass for a just-created row (real-MLS
        bootstrap in particular) — clicking the instant state confirms the
        index exists raced a not-yet-attached ``conversation-item`` and 404'd
        (testing.md convention 14: deadline-poll the UI catching up to state,
        never assume the two are simultaneous).

        A just-selected thread can also never grow a ``conversation-item`` row
        at all: an app whose conversations page is a single-pane push stack
        (apple/iOS) navigates straight to the newly-selected thread's detail —
        ``send_new_thread`` selects before it sends — covering the list root
        before the freshly-inserted row ever mounts, so its slot never
        registers and the count stays stuck.
        Apps that publish ``data.selected_thread_id`` let this poll also recognize
        "already open" by identity — matching this opener's own contract of
        never resolving by label — instead of only by row click; apps that
        don't publish it (the field reads ``None``) fall through to the
        original row-click wait unchanged.
        """
        self._ensure_on_conversations_page()
        threads = self.list_threads()
        for i, t in enumerate(threads):
            if t.thread_id == thread_id:
                deadline = time.time() + timeout_s
                next_selection_check = 0.0
                while time.time() < deadline:
                    if self.driver.count("conversation-item") > i:
                        self.driver.click("conversation-item", index=i)
                        try:
                            self._wait_thread_open()
                        except AssertionError as e:
                            raise AssertionError(
                                f"{e} {self._clicked_row_diagnosis(i, thread_id, threads)}"
                            ) from None
                        return
                    now = time.time()
                    if now >= next_selection_check:
                        next_selection_check = now + 0.5
                        try:
                            self.driver.set_state({})
                        except Exception:
                            pass
                        if (self.driver.get_state("data.selected_thread_id") == thread_id
                                and self.driver.is_visible("thread-header")):
                            self._wait_thread_open()
                            return
                    time.sleep(0.1)
                # Convention 6: name WHICH of the two render failures this is
                # before the caller has to guess. `rendered` vs `len(threads)`
                # separates "the list root is covered / never mounted" (rendered
                # == 0) from "the list renders a STALE row set" (0 < rendered <=
                # i) — two different bugs that used to share one message.
                rendered = self.driver.count("conversation-item")
                raise AssertionError(
                    f"conversation-item[{i}] never rendered within {timeout_s}s "
                    f"even though list_threads() state already reports thread "
                    f"{thread_id!r} at that index — UI render/registration "
                    f"lagged the state push; "
                    f"rendered={rendered} of {len(threads)} state threads, "
                    f"selected_thread_id="
                    f"{self.driver.get_state('data.selected_thread_id')!r}, "
                    f"state ids={[t.thread_id for t in threads]}, "
                    f"registry tree=\n{self.driver.tree()}"
                )
        raise AssertionError(
            f"No thread with id {thread_id!r}; have "
            f"{[(t.thread_id, t.label) for t in threads]}"
        )

    def _clicked_row_diagnosis(self, index: int, thread_id: str, threads: list) -> str:
        """Failure-path only (e2e rule 6): the header never appeared after a row
        click, so say what the click reached.

        ``selected_thread_id`` splits the cases. ``None`` means the click selected
        nothing, or the selection was dropped again (only if the app publishes the
        field). Another id means the click opened a different thread than the one
        state put at ``index``, so rendered order and state order disagree. Our own
        id means the selection landed and only the detail pane failed to render.
        The rendered row texts beside the state labels show which row ``index``
        actually addressed.
        """
        parts: list[str] = [f"Clicked conversation-item[{index}] for thread {thread_id!r}."]

        def probe(label: str, fn) -> None:
            try:
                parts.append(f"{label}={fn()!r}")
            except Exception as e:  # noqa: BLE001 — diagnostic only; never mask the failure
                parts.append(f"{label}=<{type(e).__name__}: {e}>")

        def selected() -> object:
            self.driver.set_state({})
            return self.driver.get_state("data.selected_thread_id")

        probe("selected_thread_id", selected)
        probe("rendered rows", lambda: [t[:60] for t in self.driver.get_texts("conversation-item")[:index + 3]])
        parts.append(f"state labels={[t.label[:60] for t in threads[:index + 3]]!r}")
        return " ".join(parts)

    def open_thread_with_subject(self, subject: str):
        """Find a thread by its label / subject and click the matching
        ``conversation-item`` row. Navigates to the conversations page first if
        the user isn't already there. Prefers an exact label match, else a
        substring match.

        ⚠ **A needle is only safe when it is unique across the WHOLE run** (a
        per-test nonce, say) — threads accumulate in the session-scoped nest, so
        a subject another test also uses is not a handle on *your* thread. If you
        are opening a thread you just injected, call ``inject_and_open_thread``
        instead; it resolves by identity and cannot collide. This helper now
        **refuses to guess** between several matches rather than silently opening
        the first one.
        """
        self._ensure_on_conversations_page()
        threads = self.list_threads()
        exact = [
            (i, t) for i, t in enumerate(threads) if (t.label or "") == subject
        ]
        loose = [
            (i, t)
            for i, t in enumerate(threads)
            if subject.lower() in (t.label or "").lower()
        ]
        self._click_sole_match(exact or loose, f"subject {subject!r}", threads)

    def open_thread_for_sender(self, sender: str):
        """Find a thread by participant / snippet match and click the row.

        ⚠ Same accumulation caveat as ``open_thread_with_subject`` — and it bites
        harder here, because bare needles like ``"bob"`` or ``"carol"`` are shared
        by several test files. Opening a thread you just injected? Use
        ``inject_and_open_thread``.
        """
        self._ensure_on_conversations_page()
        threads = self.list_threads()
        needle = sender.lower()
        # Per spec a 1:1 thread's label is the participant displays joined
        # — sender's typed address (e.g. "alice@self-nest.test") shows up
        # in the label or in the participant chip set.
        matches = [
            (i, t)
            for i, t in enumerate(threads)
            if needle in (t.label or "").lower()
            or needle in (t.snippet or "").lower()
        ]
        self._click_sole_match(matches, f"sender {sender!r}", threads)

    def _click_sole_match(self, matches: list, what: str, threads: list) -> None:
        """Click the one matching conversation row, or fail loudly.

        Refusing to pick among several matches is the point: opening whichever
        thread happened to sort first is how a needle collision turns into a
        *silent wrong-thread* read that still asserts plausibly. A red here means
        the needle is ambiguous — resolve by identity (``inject_and_open_thread``
        / ``open_thread_by_id``) rather than making the needle longer.
        """
        listing = [(t.thread_id, t.label, t.snippet) for t in threads]
        if not matches:
            raise AssertionError(
                f"No thread found matching {what}. The needle may have been "
                "invalidated by another test in this session-scoped run (a rename "
                "replaces the label; a later inbound replaces the snippet) — open "
                f"the thread by identity instead. Threads now: {listing}"
            )
        if len({t.thread_id for _, t in matches}) > 1:
            raise AssertionError(
                f"{what} is AMBIGUOUS — it matches {len(matches)} threads "
                f"{[(t.thread_id, t.label) for _, t in matches]}. Refusing to "
                "guess; open the thread by identity (inject_and_open_thread / "
                f"open_thread_by_id). Threads now: {listing}"
            )
        index = matches[0][0]
        self.driver.click("conversation-item", index=index)
        self._wait_thread_open()

    def open_thread_by_channel(self, channel_hex: str):
        """Open the thread bound to MLS channel ``channel_hex`` by clicking its
        ``conversation-item`` row — the unambiguous pick when a session-scoped
        identity has accumulated several FaunaMls threads (e.g. earlier tests'
        restored 1:1s riding the same cross-device replica), where
        ``open_thread_by_rail`` would open whichever sorts first."""
        self._ensure_on_conversations_page()
        threads = self.list_threads()
        for i, t in enumerate(threads):
            if getattr(t, "channel_id_hex", None) == channel_hex:
                self.driver.click("conversation-item", index=i)
                self._wait_thread_open()
                return
        raise AssertionError(f"No thread bound to channel {channel_hex!r}")

    def open_thread_by_rail(self, rail: str, snippet: str | None = None):
        """Open a thread on ``rail`` by clicking its ``conversation-item`` row.
        Used when the participant display isn't a stable needle for
        ``open_thread_for_sender`` (e.g. a peer registered by raw 64-hex actor id,
        whose 1:1 label is the actor id rather than a handle). Navigates to the
        conversations page first.

        ``snippet`` disambiguates the pick, and callers that know what the thread
        says should pass it. Without it this opens whichever thread on the rail
        sorts FIRST — the precise hazard ``open_thread_by_channel`` documents: a
        session-scoped identity accumulates several FaunaMls threads (earlier
        tests' 1:1s riding the same replica), so the first match is often not the
        one the caller just caused. That is how
        ``test_mark_as_spam_writes_sealed_history_row_then_undo`` failed on tui
: its own receive-wait had already
        confirmed a thread whose snippet held the spam body, then this opened a
        different one and the bubble count came back 0.

        The failure is self-diagnosing (convention 6): it names every thread and
        rail present, so "opened the wrong thread" never again presents as "the
        message never rendered"."""
        self._ensure_on_conversations_page()
        threads = self.list_threads()
        candidates = [(i, t) for i, t in enumerate(threads) if t.rail == rail]
        if snippet is not None:
            candidates = [
                (i, t) for i, t in candidates if snippet in (t.snippet or "")
            ]
        if not candidates:
            listing = [(t.rail, t.snippet) for t in threads]
            raise AssertionError(
                f"No thread found on rail {rail!r}"
                + (f" whose snippet contains {snippet!r}" if snippet else "")
                + f". Threads now: {listing}"
            )
        index = candidates[0][0]
        self.driver.click("conversation-item", index=index)
        self._wait_thread_open()

    def reveal_remote_content(self, index: int = 0) -> None:
        """Click a message's ``load-remote-content-button`` to reveal its blocked
        remote images (html-mail Slice 3). The button is per-message and indexed;
        ``index`` selects which bubble's button when several are shown."""
        self.driver.click("load-remote-content-button", index=index)

    # ── attachments in the open thread (conversations.md § Attachments) ──

    def attachment_image_states(self) -> list[str]:
        """``painted`` / ``placeholder`` for each ``dm-attachment-image`` in the open
        thread — the attachment twin of ``FeedActions._post_image_states_in``, read
        from the same per-app sources: tui's picture IS the element's text (a ``▀``
        per cell, ``apps/fauna-tui/src/thumbnail.rs`` HALF_BLOCK) and its declared
        placeholder (``name (size)``) carries none; every other app answers
        ``get_attr(..., "state")``. A ``None`` prints as such and is not ``painted``."""
        d = self.driver
        n = d.count("dm-attachment-image")
        if d.is_tui():
            return [
                "painted" if "▀" in (d.get_text("dm-attachment-image", index=i) or "") else "placeholder"
                for i in range(n)
            ]
        return [str(d.get_attr("dm-attachment-image", "state", i)) for i in range(n)]

    def evict_attachment(self, thread_id: str, filename: str) -> None:
        """Drop this device's cached bytes of every attachment named ``filename`` in
        ``thread_id`` the way the store's budget eviction does (the shared
        ``evict_thread_attachments_for_test``) — the bytes go, where they rest is
        remembered. Stands in for filling the 128 MiB store; the app refuses the
        command when nothing was evicted, so a render asserted afterwards is never
        asserted over bytes that never left."""
        self.driver.call_command(
            "conversations_evict_attachment", {"thread_id": thread_id, "filename": filename}
        )
        self._assert_command_honoured("conversations_evict_attachment")

    # ── post-succession member review (identity-succession.md § Propagation →
    # *MLS groups*, item 3a). The pair renders on the member chips of the open
    # thread, for members the succession sweep could vouch for nothing about.

    def member_chip_count(self) -> int:
        """How many `thread-member-chip` rows the open thread renders."""
        return self.driver.count("thread-member-chip")

    def member_unattested_mark_visible(self, index: int = 0) -> bool:
        """Whether the member chip at `index` carries
        `thread-member-unattested-mark`.

        ⚠ **Scoped to the chip, never read flat.** The pair renders only on
        *flagged* members, so a flat read would index the marks `0..n` over a
        chip list `0..m` — `thread-member-unattested-mark[0]` would be the first
        flagged member while `thread-member-chip[0]` is the first member, and the
        two would silently disagree about who is being asked about. tui's
        `member_review_elements` scopes it for exactly this reason.

        ⚠ **Read as PRESENCE in the tree (`not is_absent`), never as a viewport
        read.** The mark is a `Visibility`-gated element nested inside its chip,
        and the chip sits in a header row narrower than the review copy: on
        windows `is_visible` is `!IsOffscreen`, so a correctly painted mark that
        clips past the header's edge answered `False` (convention 7's rider on
        positive reads; the roster held the review and `chips=1` the whole
        window). On every other app `is_absent` IS `not is_visible`, so nothing
        else changes.
        """
        return not self.driver.is_absent(
            "thread-member-unattested-mark",
            scope=f"thread-member-chip[{index}]",
        )

    def any_member_unattested_mark_visible(self) -> bool:
        """Whether ANY member chip of the open thread is flagged.

        The chip index of a given participant is not something a journey should
        have to predict, so this is the honest question for "the successor's
        first session raises the review" — the per-chip reader above is for a
        test that has already established which chip it means.
        """
        return any(
            self.member_unattested_mark_visible(i)
            for i in range(self.member_chip_count())
        )

    def member_unattested_mark_text(self, index: int = 0) -> str:
        """The flagged chip's review copy, or `""` when that chip is not flagged."""
        try:
            return self.driver.get_text(
                "thread-member-unattested-mark",
                scope=f"thread-member-chip[{index}]",
            )
        except Exception:
            return ""

    def keep_member(self, index: int = 0) -> None:
        """Press **Keep** on the flagged member chip at `index`.

        Records a `Kept` verdict against that person's review item. Remove has no
        twin here — the chip itself already is it (`remove_member` below),
        exactly as `nest-trust-grant-revoke` is the grant plane's.
        """
        self.driver.click(
            "thread-member-keep-button",
            scope=f"thread-member-chip[{index}]",
        )

    def remove_member(self, index: int = 0) -> None:
        """Tap `thread-member-chip[index]` to REMOVE that participant.

        ⚠ **The chip's tap is capability-split, so this is only a removal on a
        `supports_membership_change` thread** (`conversations.md` § the
        `thread-member-chip[i]` row; § Participants vs reply recipients). On
        mail the same tap reveals the participant's full address and removes
        nothing — you cannot un-send who an email went to — so calling this
        against a mail thread asserts nothing and silently passes. Every app
        makes the same either/or split at paint: tui emits a `gesture_button`
        vs a plain `label`, linux attaches the remove gesture vs a popover, and
        apple's `MemberChip` branches on `removable`.

        Pick `index` off `ThreadSummary.participant_actor_ids`, which is
        index-parallel with the chip order — the chip's own text is a contact
        display name and two members can share one.
        """
        self.driver.click("thread-member-chip", index=index)

    def _wait_add_participant_dialog_closed(self, timeout_s: float = 10.0) -> None:
        """Wait for the add-participant dialog to close after a confirm click.

        The dialog hosts ``recipient-picker-input``; once ``confirm_add_participant``
        applies the membership change and clears ``snapshot.add_participant``, the
        detail pane's render closes the dialog and the input leaves the
        accessibility tree. Polling that disappearance is the deterministic proxy
        for "the membership mutation has been applied" — see the call site in
        ``add_participant_to_thread``.

        Two flakiness sources under full-suite contention force a *debounced*
        wait (the full-suite-only red on ``test_add_to_oneonone_forks_new_group``
        at 94%, where the caller's immediate ``list_threads()`` raced a
        still-pending confirm and read pre-fork state):

        1. The bridge's ``do_is_visible`` catches *every* server-side AT-SPI
           error and returns ``False`` — so a transient ``_find_element`` failure
           while the dialog is still open looks identical to a real close.
        2. The client ``is_visible`` can raise a transient transport error
           (``RuntimeError``/``BridgeDead``) that the previous code wrongly
           treated as "closed".

        So: only conclude "closed" after the input reads absent for two
        *consecutive* polls; reset the streak on a visible read or a transient
        error. A genuine close yields sustained absence; a transient blip yields
        a single one that the next poll clears.

        Raises ``AssertionError`` — self-diagnosing (e2e rule 6) — if the dialog
        is still open when ``timeout_s`` elapses, rather than returning as if the
        membership mutation had applied (`e2e-conventions.md` convention 6 rider).
        """
        deadline = time.time() + timeout_s
        consecutive_closed = 0
        last_visible: bool | str = "<never read>"
        while time.time() < deadline:
            try:
                visible = self.driver.is_visible("recipient-picker-input")
                last_visible = visible
            except LookupError:
                # Element genuinely gone from the a11y tree — dialog closed.
                return
            except Exception as e:  # noqa: BLE001 - transient bridge/transport error
                # Transient bridge/AT-SPI transport error — reset, keep waiting.
                last_visible = f"<read raised {e!r}>"
                consecutive_closed = 0
                time.sleep(0.1)
                continue
            if not visible:
                consecutive_closed += 1
                if consecutive_closed >= 2:
                    return
            else:
                consecutive_closed = 0
            time.sleep(0.1)
        raise AssertionError(
            f"add-participant dialog did not close within {timeout_s}s "
            f"(recipient-picker-input last read: {last_visible!r}; "
            f"diagnose={self.driver.diagnose('recipient-picker-input')}). "
            f"The membership mutation cannot be assumed to have applied."
        )

    def _wait_thread_open(self, timeout_s: float = 5.0) -> None:
        """Wait for the detail-pane ThreadHeader to appear after clicking a
        conversation-item. The selection round-trip is async (UniFFI observer
        → DispatcherQueue.TryEnqueue → Refresh), so without this wait the
        next action (rename, add participant) races the SetCapabilities
        call that flips the rename / membership buttons from Collapsed to
        Visible.

        ⚠ **This waits for page CHROME only — it is not, and cannot be, a wait
        for a thread's CONTENT.** The header renders in well under a second; the
        messages inside it can take far longer. A caller that reads what it
        asserts straight after this call has written a zero-wait read against a
        proxy that renders earlier — the exact trap
        `e2e-latency-independent-assertions.md` names. Measured 2026-09-11 on
        windows: a ~3 MiB mail body's bubble appears **40.7 s** after the thread
        opens, so `test_mail_client_receive_over_frame_reference.py` read
        `count(...) == 0` at ~5 s and stayed red 100 % of the time — which read
        as a broken chunk-rejoin for two days precisely because this wait used to return silently instead of
        naming what it had and had not observed.

        So: deadline-poll the state you actually assert, under its own named
        budget, after calling this. Do not widen `timeout_s` instead — the
        header really is there; the content is what you are waiting for.

        Raises ``AssertionError`` — self-diagnosing (e2e rule 6) — if
        ``thread-header`` never becomes visible within ``timeout_s``, rather
        than letting a caller proceed as if selection had completed
        (`e2e-conventions.md` convention 6 rider).
        """
        deadline = time.time() + timeout_s
        last_error: Exception | None = None
        while time.time() < deadline:
            try:
                if self.driver.is_visible("thread-header"):
                    return
                last_error = None
            except Exception as e:  # noqa: BLE001 - reported on timeout, never masks it
                last_error = e
            time.sleep(0.1)
        detail = f" (last is_visible raised {last_error!r})" if last_error else ""
        # `frame` is what classifies a present-but-unseeable header (count=1,
        # visible=False): an empty rect is a title column squeezed to nothing,
        # a real rect outside the window is a pane pushed off it. It named the
        # cause first time out on windows (2026-09-14: `-172,-172,0,0` beside
        # on-screen header buttons).
        raise AssertionError(
            f"thread-header did not become visible within {timeout_s}s{detail}; "
            f"diagnose={self.driver.diagnose('thread-header', attrs=('frame',))}. The detail-pane "
            f"selection round-trip cannot be assumed to have completed."
        )

    def _ensure_on_conversations_page(self) -> None:
        """Navigate to the conversations page and wait for the list to
        bind. Always re-navigates so the observer fires its first tick
        after the page is loaded — without that, the snapshot might be
        applied while the page is still hidden and the list never
        materializes (WinUI ItemsSource bind defers until layout pass).
        """
        self.navigate()
        # Wait for the first conversation-item to render after the
        # observer's initial Refresh. List may legitimately be empty if
        # no inbound has been injected yet — callers don't depend on this,
        # they assert on list_threads first.
        deadline = time.time() + 5.0
        while time.time() < deadline:  # deadline-ok: an empty list is legal — callers assert on list_threads, not this wait
            try:
                count = self.driver.count("conversation-item")
                if count and count > 0:
                    return
            except Exception:
                pass
            time.sleep(0.15)

    def create_mls_group(self, members: list[str]) -> str | None:
        """Create an MLS group thread containing the given members.

        Test-helper path: dispatches to the manager's ``create_mls_group``
        which constructs the thread directly without welcome / key-package
        distribution. The per-rail follow-on (the shared-Rust conversations-rails
        follow-up) replaces this with a real MLS welcome flow. The created thread is
        opened automatically so the caller can immediately reach its
        ``thread-rename-button`` / ``thread-add-participant-button``
        affordances — those buttons only render when a thread is selected.

        ⚠ **NOT an MLS witness — this seam creates NO crypto state.** It is a
        thread-store fixture over ``MockRailBackend``
        (``libs/fauna-conversations/src/manager.rs``): it materialises a rendered
        thread and touches the MLS *engine* not at all, so the openMLS
        ``provider`` gains no channel and nothing lands in the ``__mls`` replica
        at rest. A test whose precondition is *"real MLS state exists"* — a
        replica re-seal, a cross-device restore, a succession's leg 3 — is
        **vacuous or red** when seeded here, and the red looks exactly like a
        broken product leg. Measured 2026-08-31: the succession journey
        ``test_the_successors_conversations_unlock_without_a_user_command`` spent
        four investigation passes on a leg that had been green since 2026-08-21,
        because an instrumented run finally reported ``provider_ours=true
        channels=0 histories=0 owed=0`` — the re-seal loop never ran an
        iteration. Use ``enable_real_faunamls()`` + ``real_resolve_send_new()``
        instead, and gate on ``helpers.conversations_restart.wait_replica_settled``
        so the debounced autosave has actually flushed.
        """
        # Prepend "me@self-nest.test" so the group thread reflects the
        # logged-in user as a participant — production MLS groups always
        # include self in the participant set, and tests assert against
        # that count (e.g. test_add_to_mls_group_stays_in_thread expects
        # 3 base + 1 added = 4).
        all_members = ["me@self-nest.test"] + [m for m in members if m != "me@self-nest.test"]
        # Identify the group we create by DIFF, not by taking the last MlsGroup row:
        # threads accumulate across this session-scoped run, so "the last group" is the
        # last one to *sort*, which is not necessarily the one we just made.
        before = {t.thread_id for t in self.list_threads()}
        self.driver.call_command("conversations_create_mls_group", {
            "participants": all_members,
        })
        threads = self.list_threads()
        fresh = [
            (i, t)
            for i, t in enumerate(threads)
            if t.thread_id not in before and t.flavor == "MlsGroup"
        ]
        if not fresh:
            return None
        index, thread = fresh[-1]
        self._ensure_on_conversations_page()
        self.driver.click("conversation-item", index=index)
        self._wait_thread_open()
        return thread.thread_id

    # --- Real-wire FaunaMls (tier_3 test_fauna_mls_real_roundtrip; linux) ---
    #
    # Opt the client into the REAL FaunaMls backend and drive its async manager
    # wire-drivers with the API-tier peer's actor_id injected (the
    # resolve_address handle→actor chain stays deferred). The matching bridge
    # commands live in apps/fauna-linux/src/main.rs; the backend opt-in is in
    # conversations/conv_backend.rs (a linux conversations-membership follow-up).
    # block_on'd nest round-trips run on the GTK command thread, so these pass a
    # generous command timeout.

    #: The ack budget for a `real_*` command. **A ceiling, not an expectation** —
    #: `call_command` deadline-polls the app's own `last_command_id`, so a green
    #: run returns the moment the app acks and pays nothing for the headroom
    #: (testing.md § conventions, point 14).
    #:
    #: ⚠ **30 s is enough, and a raise does NOT fix the current red — measured,
    #: not assumed (2026-08-15).** `conversations_real_resolve_send_new` was
    #: failing to ack here, and "five nest round-trips `block_on`'d on the GTK
    #: command thread, on a loaded box" is a seductive explanation: it is exactly
    #: the wall-clock brittleness convention 14 warns about. It is also wrong. The
    #: budget was raised to 180 s and the command **still** did not ack — at 30 s
    #: and at 180 s alike, and identically for the unmodified-on-`origin/main`
    #: test. So the app is not slow, it is not acking, and the raise bought
    #: nothing but reds that take three times as long to surface. Reverted.
    #:
    #: Recorded because the next reader will have the same idea: if you are here
    #: because a `real_*` command timed out, raising this number is the one fix
    #: already ruled out.
    _REAL_CMD_TIMEOUT = 30.0

    #: Ack ceiling for ONE activation probe. A ceiling, not an expectation —
    #: `call_command` deadline-polls the app's own `last_command_id`, so a green
    #: run returns the moment the app acks and pays nothing for the headroom
    #: (testing.md § conventions, point 14).
    _ACTIVATION_PROBE_TIMEOUT = 30.0

    def enable_real_faunamls(self, timeout_s: float = 60.0) -> None:
        """Activate the real FaunaMls backend and wait until it's running.

        Order-independent handshake: the command sets `requested`; AuthSuccess
        (fired once per process on first login) stashes the deps; whichever
        lands second activates. Polls `data.conv_real_backend_active` until the
        backend is registered + its inbound task spawned.

        ⚠ **A slow ack is not a failed activation** — this loop used to let one
        `call_command` timeout raise straight out of it, so a helper that read as
        "poll for 20 s" actually died on the first probe the app was too busy to
        ack inside `call_command`'s 5 s default. That fires exactly when a fresh
        app is still finishing login (sync-agent start, index open, the bridges
        RPC burst) on a loaded box: observed 2026-08-16, the app healthy and
        logging continuously on both sides of the deadline. The probe is
        idempotent (a readiness *query* since the real backend became
        unconditional at login), so the honest shape is a deadline poll that
        tolerates a slow probe and reports the last one's diagnostics if the
        whole budget elapses.

        This is **not** the raise-the-timeout move `_REAL_CMD_TIMEOUT` above rules
        out. That one was ruled out because the command never acked *at all* —
        identically at 30 s and at 180 s, which no budget can fix. Here the app
        demonstrably acks; the bug was a poll loop that could not survive its own
        slowest iteration.

        ⚠ **Launch-gate apps have neither half of this handshake**, so for them
        this is a no-op rather than a poll. macOS / iOS / android register the
        real backend unconditionally at process launch behind
        `FAUNA_E2E_REAL_CONVERSATIONS` (set by `conftest`'s
        `_apply_real_conversations_env` for an invocation that collects a
        `@pytest.mark.real_conversations` test), and they implement **no**
        `conversations_enable_real_faunamls` command and publish **no**
        `data.conv_real_backend_active` — the taxonomy `real_faunamls_app`'s own
        docstring spells out. Polling them here cannot ever succeed: the app logs
        `[TestAgent] unknown command` and the loop burns its whole budget before
        raising, which is what `test_identity_succession_ceremony.py`'s group test
        did on its first `--app macos` run (2026-08-22) — a 60 s failure that read
        like a broken backend and was a driver calling a door that is not there.
        The branch belongs here and not in a test file (convention 3/7). Nothing
        is weakened by returning: these apps have no client-side "is it real yet"
        signal at all, so a consuming test's own nest-side assertions are the
        readiness check — this one asserts the sweep `ran` over `groups >= 1`,
        which a mock backend cannot satisfy.

        ⚠ **But "no-op" must not mean "silent".** A launch-gate consumer that
        forgot `@pytest.mark.real_conversations` runs on the mock, and this call
        used to return as if it had asked for the real rail — so the red landed
        two helpers later as a thread that never appeared
        (`test_identity_succession_aftermath.py`'s member-review journey, 3/3 on
        macOS 2026-09-24, and green only when a marked sibling shared its
        invocation). The branch therefore asks the app's own log instead, the
        same witness `real_faunamls_app` uses (`helpers/real_rail_control.py`),
        and a closed gate raises here, naming the missing marker.
        """
        if self.driver.is_macos() or self.driver.is_ios() or self.driver.is_android():
            from helpers import real_rail_control

            real_rail_control.witness_real_rail(
                self.driver, context="`conversations.enable_real_faunamls()`"
            )
            return
        deadline = time.time() + timeout_s
        last_timeout: Exception | None = None
        while time.time() < deadline:
            try:
                self.driver.call_command(
                    "conversations_enable_real_faunamls", {},
                    timeout=self._ACTIVATION_PROBE_TIMEOUT,
                )
            except TimeoutError as e:
                last_timeout = e  # a busy app is not a dead one — keep polling
            else:
                if self.driver.get_state("data.conv_real_backend_active"):
                    return
            time.sleep(0.5)
        detail = f" Last probe: {last_timeout}" if last_timeout is not None else ""
        raise AssertionError(
            f"real FaunaMls backend did not activate within {timeout_s}s.{detail}"
        )

    def disable_real_faunamls(self) -> None:
        """Restore the FaunaMls mock backend (test-ordering safety net).

        Activation replaces the mock for the rest of the session-cached app, so a
        snapshot conversations test running after the real-wire test would hit
        the real backend. Call this in teardown to flip the FaunaMls entry back.

        Same launch-gate branch as `enable_real_faunamls`, for the same reason and
        with one extra: on those apps there is nothing to restore either, because
        the gate is the process's own env and the mock was never in place.
        """
        if self.driver.is_macos() or self.driver.is_ios() or self.driver.is_android():
            return
        self.driver.call_command("conversations_disable_real_faunamls", {})

    # The marker every app's convention-11 refusal slot writes. Matching on it
    # rather than on "the error box is non-empty" is deliberate: a *product*
    # error a test meant to provoke must not be mistaken for an agent failure.
    _AGENT_FAILURE_MARKERS = ("test agent command", "test agent refused command")

    def _assert_command_honoured(self, action: str) -> None:
        """Raise if the app reported that ``action`` failed.

        The real-wire commands used to log their error and ack success, so a nest
        refusal ("forbidden: recipient is not accepting new conversations")
        reached no test and surfaced two assertions later as a missing effect —
        which reads as a half-completed MLS bootstrap, not a policy refusal, and
        cost a full triage pass. ``e2e-conventions.md`` § convention 11: honour
        the command or fail loudly on the app's own ``error-message``.

        Reads the shared ``messages.error`` slot, so this needs no page: the apps
        route an agent failure to a nav-independent slot precisely because a
        refusal is not a property of whatever page happens to be on screen.
        """
        try:
            shown = self.driver.get_state("messages.error")
        except Exception:
            return  # a driver with no state protocol: nothing to read, no verdict
        shown = str(shown or "")
        if any(marker in shown for marker in self._AGENT_FAILURE_MARKERS):
            raise AssertionError(
                f"the app refused or failed {action!r} and said so on its own "
                f"error-message: {shown}"
            )

    def real_resolve_send_new(self, recipient: str, body: str) -> None:
        """Start a new FaunaMls conversation by *resolving* the typed `recipient`
        through the real backend probe (no actor_id injection): types `recipient`
        into the picker, drives `resolve_recipient` (which probes
        `fauna.conversations.keypackage.count` to promote a 64-hex actor id to a
        `Fauna` chip), commits it, then sends `body` (group bootstrap: fetch key
        package → create group → deliver Welcome → post Application envelope)."""
        self.driver.call_command("conversations_real_resolve_send_new", {
            "recipient": recipient,
            "body": body,
        }, timeout=self._REAL_CMD_TIMEOUT)
        self._assert_command_honoured("conversations_real_resolve_send_new")

    def real_send(self, thread_id: str, body: str) -> None:
        """Send `body` on an existing thread (a forked-but-unbound group
        bootstraps its MLS group lazily here)."""
        self.driver.call_command("conversations_real_send", {
            "thread_id": thread_id,
            "body": body,
        }, timeout=self._REAL_CMD_TIMEOUT)
        self._assert_command_honoured("conversations_real_send")

    def real_send_attachment(
        self,
        thread_id: str,
        body: str,
        filename: str,
        mime_type: str,
        data_base64: str,
    ) -> None:
        """Send `body` plus one attachment on an existing thread over the REAL
        FaunaMls rail: the manager seals the bytes under the channel epoch blob
        key and uploads the sealed blob to the nest's content-addressed
        ``/api/v1/blob`` (``ConversationsRpc::blob_put``). The receiver GETs +
        opens + renders it (``dm-attachment-image``) — the cross-engine
        attachment round-trip, not a local inject. ``docs/goal/ui/conversations.md``
        § Attachments."""
        self.driver.call_command("conversations_real_send_attachment", {
            "thread_id": thread_id,
            "body": body,
            "filename": filename,
            "mime_type": mime_type,
            "data_base64": data_base64,
        }, timeout=self._REAL_CMD_TIMEOUT)
        self._assert_command_honoured("conversations_real_send_attachment")

    def real_add(self, thread_id: str, peer_actor_id_hex: str, peer_handle: str) -> None:
        """Add the peer to `thread_id` (bound group → MLS Commit + Welcome;
        1:1 → forks a fresh group, snapshot-only until its first `real_send`)."""
        self.driver.call_command("conversations_real_add", {
            "thread_id": thread_id,
            "peer_actor_id_hex": peer_actor_id_hex,
            "peer_handle": peer_handle,
        }, timeout=self._REAL_CMD_TIMEOUT)
        self._assert_command_honoured("conversations_real_add")

    def real_remove(self, thread_id: str, peer_actor_id_hex: str, peer_handle: str) -> None:
        """Remove the peer from a bound FaunaMls group (posts the MLS Commit;
        no Welcome). `peer_handle` must match the one used at `real_add`."""
        self.driver.call_command("conversations_real_remove", {
            "thread_id": thread_id,
            "peer_actor_id_hex": peer_actor_id_hex,
            "peer_handle": peer_handle,
        }, timeout=self._REAL_CMD_TIMEOUT)
        self._assert_command_honoured("conversations_real_remove")

    def real_rename(self, thread_id: str, label: str) -> None:
        """Rename a bound FaunaMls group (posts the encrypted
        `GroupMeta::NameChanged` Application envelope)."""
        self.driver.call_command("conversations_real_rename", {
            "thread_id": thread_id,
            "label": label,
        }, timeout=self._REAL_CMD_TIMEOUT)
        self._assert_command_honoured("conversations_real_rename")

    # --- Reactions & message-delete (conversations.md § Reactions & message delete) ---

    def open_message_actions(self, message_index: int = 0, timeout_s: float = 5.0) -> None:
        """Open the per-bubble ⋯ overflow flyout (``dm-message-actions-button[i]``).

        The button is Collapsed (no UIA peer) when neither ``supports_reactions``
        NOR ``supports_message_delete && msg.is_own`` holds — on a capabilities-off
        thread you'd wait forever.  The caller is responsible for opening a thread
        that has the relevant capability set.

        Windows: the ⋯ ``Button``'s click opens a ``MenuFlyout``; FlaUI's Invoke
        on the Button triggers the Flyout.  The flyout items appear in a popup
        window — FlaUI finds them globally (they're children of the popup root,
        not the main window), so the caller can click ``dm-reaction-option`` /
        ``dm-message-delete-button`` by AutomationId after this returns.
        """
        deadline = time.time() + timeout_s
        while time.time() < deadline:
            try:
                if self.driver.count("dm-message-actions-button") > message_index:
                    self.driver.click("dm-message-actions-button", index=message_index)
                    return
            except Exception:
                pass
            time.sleep(0.1)
        raise AssertionError(
            f"dm-message-actions-button[{message_index}] never appeared within "
            f"{timeout_s}s — is the thread open with a capability that shows the ⋯ "
            f"button? diagnose={self.driver.diagnose('dm-message-actions-button')}"
        )

    def react_to_message(
        self,
        message_index: int = 0,
        reaction_index: int = 0,
        timeout_s: float = 8.0,
    ) -> None:
        """Open the ⋯ menu on ``message_index`` and click the quick-set reaction
        ``dm-reaction-option[reaction_index]`` (0=👍, 1=❤️, 2=😂, 3=😮, 4=😢, 5=🙏).

        All per-platform driver branching for capability gating lives here, not in
        test files (e2e rule 7 / conversations.md § Architectural rules).

        Windows: ``dm-reaction-option`` items are ``MenuFlyoutItem``s that live in
        the popup UIA subtree once the flyout is open.  FlaUI finds them by
        AutomationId globally after the ⋯ button triggers the flyout open.  If FlaUI
        genuinely cannot find ``dm-reaction-option`` by AutomationId after the flyout
        opens, the caller gets an ``AssertionError`` with context — the test must
        report BLOCKED (not patch around it).
        """
        self.open_message_actions(message_index=message_index, timeout_s=timeout_s)
        # After the flyout opens, the MenuFlyoutItems appear in a popup subtree.
        # Poll briefly for them to be realized in the UIA tree.
        deadline = time.time() + timeout_s
        while time.time() < deadline:
            try:
                n = self.driver.count("dm-reaction-option")
                if n > reaction_index:
                    self.driver.click("dm-reaction-option", index=reaction_index)
                    return
            except Exception:
                pass
            time.sleep(0.1)
        raise AssertionError(
            f"dm-reaction-option[{reaction_index}] did not appear in the flyout "
            f"within {timeout_s}s after clicking dm-message-actions-button[{message_index}]. "
            f"count={self.driver.count('dm-reaction-option') if True else 0}. "
            "If this is 0 after the flyout opened, the MenuFlyoutItem is not "
            "realized by AutomationId — BLOCKED: C2 render shape cannot be driven "
            "by FlaUI via AutomationId (needs MenuFlyoutItem→Button swap or "
            "explicit AutomationPeer override)."
        )

    def react_with_custom_emoji(
        self,
        emoji: str,
        message_index: int = 0,
        timeout_s: float = 8.0,
    ) -> None:
        """React with ``emoji`` through the **fuller picker** — the one beyond the
        six quick-set options (``dm-reaction-more-button``).

        The picker WIDGET is the single sanctioned per-app divergence in this
        menu (``ui.yaml``'s ``dm-message-actions-menu`` component), so every
        platform branch lives here and a test asserts only what the outcome
        promises: an emoji outside the quick set ends up as a pill (e2e rule 7).

        The seam is the ONE id the picker already has: click it to open the
        picker, then ``type_text`` the emoji at it, and each app's automation
        layer drives its own widget's pick path from there — no id on the
        picker's cells, no ``ui.yaml`` change (conventions 1 and 7).

        **tui** — a terminal has no OS emoji picker, so "more" flips the menu
        into a free-entry prompt that re-uses the same id as its input
        (``apps/fauna-tui``'s ``ActionsStep::EmojiEntry``). It paints as a
        *committing* input, which is the type-then-click idiom: click opens the
        prompt, ``type_text`` fills it, and the second click fires the commit
        that a human reaches with Enter.

        **linux** — the click pops up the ``GtkEmojiChooser`` parented on the
        button; typing at the button emits the chooser's own ``emoji-picked``
        (the signal a human pick raises) and closes it
        (``apps/fauna-linux/src/automation/agent.rs::type_text``). A pick
        commits by itself, so there is no second click.

        **macos / ios** — "more" opens a free-entry single-emoji field (the one an
        OS emoji panel — the macOS character palette, the iOS emoji keyboard —
        fills), and while it is open the field is reachable as the button's own
        id: *entry mode*, as on tui. ``type_text`` writes the field's text, whose
        change handler toggles the first emoji and closes the field, so — unlike
        tui — the entry commits by itself and there is no second click. A type
        while the field is closed is refused, as linux refuses a closed chooser
        (``DmMessageBubble.swift``'s ``dm-reaction-more-button`` registration).

        **windows** — "more" opens a flyout holding a free-entry single-emoji
        ``TextBox`` (the field Win+. fills) above the shortcut grid; while it is
        open the field carries the button's id (*entry mode*) and the menu item
        gives it up. ``type_text`` sets the field's value (``ValuePattern``), and
        its ``TextChanged`` commits the first complete emoji and closes the
        flyout — as on apple, no second click (``DmMessageBubble.xaml.cs``'s
        ``ShowMoreReactionsPicker``).

        **android** — the click opens the sheet hosting the emoji2
        ``EmojiPickerView``, whose cells carry no test id. The driver routes the
        typed emoji to the app's agent (``drivers/android.py::type_text``), which
        picks it through the open sheet's own pick handler — the one the view's
        listener calls — so the reaction toggles through the screen's one toggle
        path and the sheet closes, as a human pick does
        (``TestAgent.kt::pickMoreReaction``). A pick commits by itself, so there is
        no second click; a type with no sheet open is refused, as on linux.

        Web alone has no seam yet: a built grid of fixed emoji, which cannot
        reach an emoji outside it, so the seam there is a UI change, not only an
        automation one. That is real parity debt, declared through
        ``skip_unbuilt`` rather than skipped silently (convention 7).
        """
        from helpers.app_surface import skip_unbuilt

        self.open_message_actions(message_index=message_index, timeout_s=timeout_s)
        is_tui = getattr(self.driver, "is_tui", lambda: False)()
        if not (
            is_tui
            or self.driver.is_linux()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_windows()
            or self.driver.is_android()
        ):
            skip_unbuilt(
                self.driver,
                surface="a drivable dm-reaction-more-button picker",
                detail=(
                    "the fuller picker has no automation seam on this app yet "
                    "(a fixed built grid on web), so no test can pick an emoji "
                    "outside the quick set here yet"
                ),
                tracked="docs/features/reactions-and-message-delete.md outcome 5",
            )
        self.driver.click("dm-reaction-more-button")
        self.driver.type_text("dm-reaction-more-button", emoji)
        if is_tui:
            self.driver.click("dm-reaction-more-button")

    def toggle_reaction_pill(self, message_index: int = 0, pill_index: int = 0) -> None:
        """Tap an existing ``dm-reaction-pill[i]`` to toggle the reaction off (or on
        if a different user placed it).  The pill is a ``Button`` under the bubble;
        tapping fires ``ReactionToggleRequested`` on the owning ``DmMessageBubble``.
        """
        self.driver.click("dm-reaction-pill", index=pill_index)

    def reaction_pill_count(self, timeout_s: float = 5.0) -> int:
        """Return the number of ``dm-reaction-pill`` Buttons currently in the UIA
        tree (across the whole open thread). Poll briefly so callers don't need
        their own wait loop after a ``react_to_message`` call.
        """
        deadline = time.time() + timeout_s
        last = 0
        while time.time() < deadline:
            try:
                last = self.driver.count("dm-reaction-pill")
                if last > 0:
                    return last
            except Exception:
                pass
            time.sleep(0.2)
        return last

    def reaction_pill_name(self, index: int = 0) -> str:
        """Return the ``AutomationProperties.Name`` of ``dm-reaction-pill[i]``,
        which the C# render sets to ``"{emoji} {count}"`` (e.g. ``"👍 1"``).
        On windows the FlaUI bridge reads the UIA Name property, which is
        exactly ``AutomationProperties.Name``.

        Caller: use ``driver.get_attr("dm-reaction-pill", "Name")`` or — since
        FlaUI's ``get_text`` on a Button reads its content TextBlock — use the
        button's Content text instead.  This helper tries ``get_text`` first
        (content TextBlock = ``"{emoji} {count}"``), which is the same string;
        falls back to ``get_attr(..., "name")`` if the content read fails (e.g.
        if the pill renders as an image on some platform).
        """
        try:
            return self.driver.get_text("dm-reaction-pill", index=index)
        except Exception:
            return self.driver.get_attr("dm-reaction-pill", "name") or ""

    def wait_for_reaction_pill(
        self, expected_count: int = 1, timeout_s: float = 8.0
    ) -> None:
        """Poll until ``driver.count("dm-reaction-pill") >= expected_count``.

        Reactions are fire-and-forget from the C# side: the ⋯-click → server RPC
        → snapshot observer → Bind() rebuild.  The round-trip is async, so callers
        need a poll gate before asserting on the pill.  Self-diagnosing on timeout.
        """
        deadline = time.time() + timeout_s
        while time.time() < deadline:
            try:
                if self.driver.count("dm-reaction-pill") >= expected_count:
                    return
            except Exception:
                pass
            time.sleep(0.2)
        raise AssertionError(
            f"expected ≥{expected_count} dm-reaction-pill(s) within {timeout_s}s; "
            f"diagnose={self.driver.diagnose('dm-reaction-pill')}"
        )

    def delete_message(self, message_index: int = 0, timeout_s: float = 10.0) -> None:
        """Delete an own message: open ⋯ → click ``dm-message-delete-button`` →
        click ``dm-message-delete-confirm-button`` in the confirm sub-flyout.

        Two flyout hops (C2 report § EXACT ⋯-flyout structure):
        1. Click ``dm-message-actions-button[message_index]`` — opens the
           ``MenuFlyout`` (``dm-message-actions-menu``).
        2. Click ``dm-message-delete-button`` (a ``MenuFlyoutItem``) — the click
           CLOSES the MenuFlyout and immediately opens a separate confirm ``Flyout``
           anchored to the ⋯ Button.
        3. Click ``dm-message-delete-confirm-button`` in the confirm Flyout.

        All three elements must be reachable by AutomationId.  If FlaUI cannot find
        ``dm-message-delete-button`` after the MenuFlyout opens, this raises an
        ``AssertionError`` with BLOCKED context.
        """
        self.open_message_actions(message_index=message_index, timeout_s=timeout_s)
        # Step 2: find and click the delete button in the open MenuFlyout.
        deadline = time.time() + timeout_s
        while time.time() < deadline:
            try:
                if self.driver.count("dm-message-delete-button") >= 1:
                    self.driver.click("dm-message-delete-button", index=0)
                    break
            except Exception:
                pass
            time.sleep(0.1)
        else:
            raise AssertionError(
                "dm-message-delete-button did not appear in the flyout within "
                f"{timeout_s}s after clicking dm-message-actions-button[{message_index}]. "
                "If count=0 after the flyout opened, the MenuFlyoutItem is not "
                "realized by AutomationId — BLOCKED: C2 render shape cannot be "
                "driven by FlaUI (needs Button swap or AutomationPeer override)."
            )
        # Step 3: the MenuFlyout dismisses and the confirm sub-Flyout appears.
        # Poll for the confirm button.
        deadline2 = time.time() + timeout_s
        while time.time() < deadline2:
            try:
                if self.driver.count("dm-message-delete-confirm-button") >= 1:
                    self.driver.click("dm-message-delete-confirm-button", index=0)
                    return
            except Exception:
                pass
            time.sleep(0.1)
        raise AssertionError(
            "dm-message-delete-confirm-button did not appear within "
            f"{timeout_s}s after clicking dm-message-delete-button. "
            f"diagnose={self.driver.diagnose('dm-message-delete-confirm-button')}"
        )

    def mark_message_spam(self, message_index: int = 0, timeout_s: float = 10.0) -> None:
        """Mark a received message as spam: open ⋯ → click
        ``dm-message-mark-as-spam-button``.

        One flyout hop (no confirm step, unlike delete):
        1. Click ``dm-message-actions-button[message_index]`` — opens the
           ``dm-message-actions-menu`` flyout.
        2. Click ``dm-message-mark-as-spam-button`` — trains the sealed tier-1
           spam model over this message's decrypted body AND writes a **sealed**
           training-history row (the live Insert consumer; mail-spam.md § Wire
           shapes ``put_spam_model`` ``history_op``). Gated ``!is_own`` — only a
           received bubble carries it, so open a thread with a peer message.
        """
        self.open_message_actions(message_index=message_index, timeout_s=timeout_s)
        deadline = time.time() + timeout_s
        while time.time() < deadline:
            try:
                if self.driver.count("dm-message-mark-as-spam-button") >= 1:
                    self.driver.click("dm-message-mark-as-spam-button", index=0)
                    return
            except Exception:
                pass
            time.sleep(0.1)
        raise AssertionError(
            "dm-message-mark-as-spam-button did not appear in the flyout within "
            f"{timeout_s}s after clicking dm-message-actions-button[{message_index}]. "
            "Gated !is_own — is the targeted bubble a received (peer) message? "
            f"diagnose={self.driver.diagnose('dm-message-mark-as-spam-button')}"
        )

    def wait_for_message_deleted(self, timeout_s: float = 8.0) -> None:
        """Poll until ``dm-message-deleted`` appears — signals the delete round-
        trip completed and the bubble flipped to the tombstone placeholder.
        """
        deadline = time.time() + timeout_s
        while time.time() < deadline:
            try:
                if self.driver.count("dm-message-deleted") >= 1:
                    return
            except Exception:
                pass
            time.sleep(0.2)
        raise AssertionError(
            f"dm-message-deleted placeholder did not appear within {timeout_s}s. "
            f"diagnose={self.driver.diagnose('dm-message-deleted')}"
        )

    # --- Internal helpers ---

    def _wait_resolve(self, timeout_s: float = 5.0):
        """Wait until recipient-resolve-status is in a terminal state (resolved or error)."""
        deadline = time.time() + timeout_s
        last_state = None
        while time.time() < deadline:
            last_state = self.driver.get_attr("recipient-resolve-status", "state")
            if last_state in ("resolved", "error"):
                return
            time.sleep(0.05)
        # Self-diagnosing failure (e2e rule 6): the bare "did not reach terminal
        # state" hid WHICH failure mode hit. Report the last `state` read AND
        # whether the element is on screen, so the message itself classifies it:
        #   - last state 'idle' + visible => resolution was NEVER INITIATED:
        #     typing into recipient-picker-input did not kick off the resolver
        #     (which would first flip resolveState to 'resolving'). Confirmed on
        #     BOTH macOS AND iOS in-process (2026-06-17) — so NOT a macOS-headless
        #     compositing artifact; an app-side gap in the shared RecipientPicker
        #     resolve wiring (RecipientPicker.swift:68 `.onChange(of: text) {
        #     onInputChange }` → its manager). A harness-correct write
        #     (automationField.setValue mutates the same $text binding). -> apple.
        #   - last state 'resolving' + visible => resolve STARTED but never
        #     completed (resolver/nest stuck).
        #   - last state '' / None + visible => platform not surfacing the `state`
        #     automation attribute.
        #   - element not visible => the picker/composer never rendered.
        # 'idle'+visible splits again on the input's OWN text (2026-09-21, the
        # first macOS `test_resolves_fauna_handle` after a cold launch read it
        # once, under load, with the real session wired and no probe in the app
        # log): text '' => the typed text was lost between the keystroke and the
        # model (the picker's `text` <-> `rawInput` resync); the typed text =>
        # it reached the field and never the manager (`onInputChange`).
        try:
            visible = self.driver.is_visible("recipient-resolve-status")
        except Exception as e:  # noqa: BLE001 - diagnostic only, never mask the real failure
            visible = f"<is_visible raised {e!r}>"
        raise AssertionError(
            f"recipient-resolve-status did not reach terminal state within "
            f"{timeout_s}s (last state read: {last_state!r}; element visible: "
            f"{visible}; {self.driver.diagnose('recipient-picker-input')}). "
            f"state 'idle'+visible => resolve never initiated (app: "
            f"onInputChange wiring, both apple clients) — the input's own text "
            f"above says whether the typed text was lost or never reached the "
            f"manager; 'resolving' => stuck "
            f"mid-resolve; ''/None => `state` attr not surfaced; not visible => "
            f"picker never rendered."
        )


class _ThreadSummary:
    """Attribute-access wrapper over a row from
    ``state.data.conversation_threads``. Tests assert on ``.label``,
    ``.flavor``, ``.snippet``, ``.message_count``, ``.participant_count``,
    ``.thread_id``, ``.rail``, ``.unread_count``.
    """

    def __init__(self, row: dict):
        self._row = row or {}

    @property
    def thread_id(self) -> str:
        return self._row.get("thread_id", "")

    @property
    def label(self) -> str:
        return self._row.get("label", "") or ""

    @property
    def snippet(self) -> str:
        return self._row.get("snippet", "") or ""

    @property
    def rail(self) -> str:
        return self._row.get("rail", "")

    @property
    def flavor(self) -> str:
        return self._row.get("flavor", "")

    @property
    def message_count(self) -> int:
        return int(self._row.get("message_count", 0) or 0)

    @property
    def participant_count(self) -> int:
        return int(self._row.get("participant_count", 0) or 0)

    @property
    def unread_count(self) -> int:
        return int(self._row.get("unread_count", 0) or 0)

    @property
    def channel_id_hex(self) -> str | None:
        """The bound nest-channel id (hex) once a FaunaMls group has
        bootstrapped, else None. Lets the real-wire test observe the channel
        an MLS thread carries on the nest."""
        return self._row.get("channel_id_hex")

    @property
    def participant_actor_ids(self) -> list:
        """Per-participant actor id (hex), index-parallel with the
        ``thread-member-chip[i]`` order; ``None`` in a slot whose participant
        is not Fauna-addressed (email, bluesky, …).

        ⚠ **The only observable of an identity succession's participant
        re-point** (``identity-succession.md`` § Propagation → *MLS groups*).
        That re-point keeps the list position **and** the handle — the nest
        moved the handle to the successor inside the succession transaction —
        so the chip's own text, the label and ``participant_count`` are all
        unchanged by it. Assert continuity on the id; asserting on chip text
        would pass equally against a client that re-pointed nothing.

        Absent (``[]``) on any app whose state serializer has not picked the
        field up yet — it ships with the shared
        ``fauna_conversations::state_json`` serializer (tui + linux); windows'
        C# twin and the four FFI apps are per-app trickle-down.
        """
        return list(self._row.get("participant_actor_ids") or [])

    @property
    def participant_displays(self) -> list:
        """What each member chip SAYS, index-parallel with
        :attr:`participant_actor_ids` and so with ``thread-member-chip[i]`` —
        straight off the app's own ``participant_displays`` column, no
        re-derivation.

        Use it for the one question the ids cannot answer: whether a seated
        member's row **names anybody**. A member seated off an MLS roster
        carries no handle (the engine roster gives actor ids and nothing else,
        ``conversation-rooms.md`` § Implementation status today), and such a
        row rendered *blank* until the Fauna arm of ``TypedAddress::display``
        gained its elided-actor-id fallback (``value-formatting.md`` § Account
        display label). ``participant_actor_ids`` shows that somebody is
        seated and who; only this shows whether the chip says anything.

        ⚠ **Never assert identity or continuity on this.** A handle is not an
        identity, and an unresolved member wears an elision of its id — key
        every membership assertion on :attr:`participant_actor_ids`. Whether a
        given seat shows a handle or an elision depends on what that device
        happens to have met, which is not a property of the room.

        Absent (``[]``) on any app whose state serializer has not picked the
        field up yet — it ships with the shared
        ``fauna_conversations::state_json`` serializer (tui + linux); windows'
        C# twin and the four FFI apps are per-app trickle-down.
        """
        return list(self._row.get("participant_displays") or [])

    @property
    def room(self) -> dict | None:
        """The thread's room facts (``conversation-rooms.md`` § The room), or
        ``None`` where the rail models no room / the app has not picked the
        field up yet: ``{"class", "my_role", "member_roles", "policy"}`` —
        ``member_roles`` is index-parallel with :attr:`participant_actor_ids`
        (and so with ``thread-member-chip[i]``), ``policy`` is ``None`` on a
        policy-less room and ``{"version", "name", "join_rule",
        "history_policy"}`` on a governed one. Enum values are their Rust
        ``Debug`` spellings (``"EndToEnd"``, ``"Owner"``, ``"Invite"``,
        ``"Full"``).

        Every governed-room assertion is a field read off this — never a
        chip-text parse, which carries a localized role mark.
        """
        room = self._row.get("room")
        return dict(room) if isinstance(room, dict) else None

    @property
    def capabilities(self) -> dict | None:
        """The thread's ``ThreadCapabilities`` as the app projects them —
        ``can_invite`` / ``can_remove_members`` / ``can_set_policy`` /
        ``can_appoint_admins`` / ``supports_rename`` / ``encryption`` — or
        ``None`` before the detail has hydrated. On a governed room the four
        role-gated flags are the roles table applied to the viewer
        (``conversation-rooms.md`` § Roles and authorization); the app greys
        the matching affordance off exactly these, never off a role it
        computed itself.
        """
        caps = self._row.get("capabilities")
        return dict(caps) if isinstance(caps, dict) else None
