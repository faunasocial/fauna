from __future__ import annotations

import time
from typing import TYPE_CHECKING

from helpers.budgets import IMAGE_PAINT_S, ORCHESTRATION_STEP_S
from helpers.waiting import await_feed_reload_after, feed_reload_baseline, feed_reloads
from i18n.strings import S

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


# Generous ceilings for the two web compose readiness gates, sized for a LOADED
# box, not an idle one (testing.md convention 14: assert latency-independent
# state; a deadline poll returns the instant the state arrives, so a green run
# pays nothing for a large budget — only a genuine hang pays).
#
# Both of these were fixed budgets tight enough that a primary dev VM at its
# normal load (many parallel builds; sustained double-digit load averages are
# the documented environment, not a spike) blew through them before the composer
# was usable — two runs of the c2pa badge test died here, at load 15 and load 22,
# neither ever reaching the assertion the test exists for. The old numbers were
# measuring the machine, not the product.
FILE_READY_BUDGET_S = 60.0
SUBMIT_ENABLED_BUDGET_S = 90.0

# The web Feed page's first load: the manager build, then `loadAll()`'s four
# reads and the local-feed reload — the same orchestration a feed select is.
FEED_FIRST_LOAD_BUDGET_S = ORCHESTRATION_STEP_S

# Selecting a feed is several nest round trips before the composed page lands
# (`FeedManager::reload` → `fauna.feed.get`, the caller's global factor set,
# each composed `topic:*` factor's sealed blob, then the scored page), so it is
# an orchestration step rather than one round trip.
FEED_SELECT_BUDGET_S = ORCHESTRATION_STEP_S

# How long one ⋯-overflow click gets to actually paint its menu before the click
# is re-issued. Deliberately small: this is a local popup with no round trip, so
# a card that has not answered in this long was re-rendered under the click and
# wants another one — a large ceiling here would spend the caller's whole
# `timeout_s` on a single swallowed click instead of retrying.
ACTIONS_MENU_OPEN_S = 2.0


class FeedActions:
    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def is_visible(self) -> bool:
        # `feed-view` is the canonical feed-page container on every app.
        # `compose-button` only exists on platforms with a collapsible
        # composer (web, iOS, Android); Linux and macOS have always-inline
        # compose and no toggle button, and on iOS `feed-view` is on a
        # non-hittable NavigationStack — so accept either.
        return self.driver.is_visible("feed-view") or self.driver.is_visible("compose-button")

    def _open_composer(self) -> None:
        """Open the post composer.

        Web and Linux have inline compose (text field always visible). iOS
        requires selecting a feed-item first (compose appears in the feed
        section). Android and the remaining native apps use a compose-button
        that navigates to / reveals the composer.
        """
        if self.driver.is_ios():
            # iOS shows compose-text-field only after a feed is selected.
            # Feeds load asynchronously — the API may still be authenticating.
            # Navigate away and back to force a fresh load of FeedListView.
            if not self.driver.is_visible("compose-text-field"):
                if not self.driver.is_visible("feed-item"):
                    self.driver.set_state({"nav": {"stack": [{"view": "contacts"}]}})
                    time.sleep(1)
                    self.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
                    self.driver.wait_for("feed-item", timeout=20)
                self.driver.click("feed-item", index=0)
                time.sleep(1)
        elif self.driver.is_android() or (not self.driver.is_web() and not self.driver.is_mobile()):
            # Android's compose-button (FeedScreen.kt's FloatingActionButton)
            # is a real navigate-away toggle to a separate compose screen
            # ("feed/compose"), not an inline reveal like iOS — the same
            # click-if-present idiom the desktop apps use below covers it.
            # Found wiring the posts-rail draft-persistence leg: every existing create_post()-family test already routes
            # through this method, so android's arm was silently missing from
            # ALL of them, not just the new draft-persistence test — masked
            # because android e2e only ever compiles on the primary dev VM and
            # has never actually RUN (host-emulator gated).
            if self.driver.is_visible("compose-button"):
                self.driver.click("compose-button")
        self.driver.wait_for("compose-text-field")

    def _attach_file(self, element_id: str, file_path: str) -> None:
        """Attach a file to an input element.

        Web: uses Playwright's set_input_files and waits for compose-file-ready.
        Native: delegates to the compose state protocol via set_input_files,
        which reaches the app's own attach seam in the test agent. That seam
        HOLDS the bytes rather than uploading them — the composer's audience is
        what decides their seal and is not known at pick time
        (`ui/media.md` § Encryption at rest), so the upload happens at submit.
        """
        self.driver.set_input_files(element_id, file_path)
        if self.driver.is_web():
            deadline = time.monotonic() + FILE_READY_BUDGET_S
            while time.monotonic() < deadline:
                if self.driver.is_visible("compose-file-ready"):
                    return
                time.sleep(0.3)
            # Falling out of this loop used to proceed silently, so a file that
            # never finished reading surfaced far downstream as a bare click
            # timeout on an unrelated element. Say it here instead
            # (testing.md convention 6).
            raise AssertionError(
                f"web: attached {file_path} to {element_id!r} but "
                f"'compose-file-ready' never appeared within "
                f"{FILE_READY_BUDGET_S:.0f}s — the SPA's FileReader never "
                f"delivered bytes, so the composer has no attachment to post. "
                f"error={self.driver_error_text()!r}"
            )

    def navigate(self) -> None:
        """Navigate to the feed page (the `{page}-tab` unified nav — e2e
        convention 3), matching every other action class's entry point."""
        self.driver.navigate_to("feed")

    def reenter(self) -> None:
        """Land on the Feed page through a real page ENTRY, so it re-queries the
        nest.

        A feed re-pulls when navigation *enters* the page — not when its tab is
        tapped while it is already the current one. On both apple apps
        ``navigate()`` from the Feed is exactly that no-op, so a post created
        behind the app's back (a nest API call, another device) is absent from
        the feed state however long the wait: measured 2026-09-21 — absent after
        12 s on macos and ios, present right after leaving and re-entering. The
        same "re-enter the page to re-poll" pattern as ``MediaActions.reenter``.
        The wait for the post itself stays a state poll
        (``wait_for_post_text``); this only guarantees a read is coming."""
        self.driver.navigate_to("conversations")
        self.driver.navigate_to("feed")

    def open_composer(self) -> None:
        """Open the post composer without composing anything — the public door to
        the private `_open_composer`, for tests that assert *on* the composer
        (draft persistence) rather than posting through it."""
        self._open_composer()

    def compose_body_text(self) -> str:
        """Return the composer's current body text (the ``compose-text-field``
        value) — used to assert a feed draft survived an app restart
        (``docs/goal/behavior/reserved-folders.md`` § Drafts Sync,
        ``docs/goal/ui/feed.md`` § Persistence). The feed twin of
        ``ConversationsActions.compose_body_text``.

        tui + linux + android + web + apple (macos/ios) today, matching the app
        legs that are wired. Each app adds its arm as its draft leg lands —
        raise loudly until then rather than silently passing, since a vacuous
        read is exactly how a draft assertion would pass on an app that
        persists nothing."""
        if self.driver.is_macos() or self.driver.is_ios():
            # macOS/iOS back `compose-text-field` with an automationField-bound
            # text view (`MacFeedDetailView.swift`/`FeedListView.swift`), the
            # same registry-direct read `ConversationsActions.compose_body_text`
            # uses for `dm-text-field` — the in-process agent's element read
            # returns the bound `composeText` verbatim, "" when empty (no
            # placeholder-scrape fallback).
            return self.driver.get_text("compose-text-field")
        if self.driver.is_linux():
            # The linux compose-text-field is a gtk::TextView (post_list.rs,
            # same widget kind as `dm-text-field`); the automation agent's
            # find.rs:text_of returns its TextBuffer text verbatim, and an
            # EMPTY composer reads back as "" (no placeholder fallback) —
            # the same registry-direct contract as tui below.
            return self.driver.get_text("compose-text-field")
        if self.driver.is_tui():
            # tui paints ``Element::input("compose-text-field", compose.text, …)``
            # (``apps/fauna-tui/src/feed/mod.rs:2684``) and ``Element::input``'s
            # ``value`` IS the element's ``text``, so the agent's element read
            # returns the draft body verbatim, and ``""`` when empty (no
            # placeholder fallback).
            return self.driver.get_text("compose-text-field")
        if self.driver.is_android():
            # Compose's compose-text-field semantics reflect the field's own
            # TextFieldValue, so a plain get_text returns the draft body
            # verbatim and "" when empty (no placeholder fallback) — the same
            # registry-direct contract as linux/tui above, and the same
            # empirically-verified Compose-semantics read
            # ConversationsActions.compose_visible_text's android arm already
            # relies on for dm-text-field.
            return self.driver.get_text("compose-text-field")
        if self.driver.is_web():
            # Unlike conversations' `dm-text-field` (a CodeMirror 6 editor that
            # virtualizes its viewport and needs the `window.__fauna_editor_docs`
            # registry read — ConversationsActions.compose_body_text's web arm),
            # feed's `compose-text-field` is a PLAIN `<textarea>`
            # (FeedComposeBar.svelte). The web bridge's generic `/element/text`
            # already special-cases `<input>`/`<textarea>`/`<select>` to read
            # `.value` (web-bridge/server.py), so no CodeMirror-style registry
            # read is needed — the same plain get_text call as tui/linux/android.
            return self.driver.get_text("compose-text-field")
        if self.driver.is_windows():
            # windows' compose-text-field is a plain WinUI TextBox — the FlaUI
            # bridge's GetText reads it via ValuePattern.Value, which IS the field's
            # live text, so this returns the draft body verbatim, and "" when empty:
            # an Edit control's empty ValuePattern is authoritative, so GetText no
            # longer falls through to scrape the template's PlaceholderText (it did
            # until 2026-09-21; witnessed on the plain-TextBox `handle-input` by
            # `test_identity_uri_handle_prefill.py`'s bare-secret case) — the same
            # contract as linux/tui/android/web above.
            return self.driver.get_text("compose-text-field")
        raise NotImplementedError(
            "feed compose_body_text is tui/linux/android/web/apple/windows-only "
            "(feed.md § Implementation status today)"
        )

    def start_submit(self) -> None:
        """Submit the staged post WITHOUT waiting for it to land — the user who
        presses Post and goes straight on to the next one (`feed.md` § User
        actions, `post-submit-button`: the composer stays editable throughout a
        submit). The caller stands inside the send (``helpers.rpc_hold`` holding
        ``fauna.posts.create``) and polls whatever it asserts on.

        * **tui** presses Enter on the button: the keyboard's activation, which
          runs the submit off the render loop exactly as a user's keypress does.
          Its agent ``click`` awaits the whole submit before replying, so nothing
          typed after it could ever land mid-send.
        * **every other app** clicks. An app whose click likewise waits for the
          whole send cannot open the window this way; a test holding the send
          then fails at its arrival wait, naming that app's gap instead of
          passing without it.
        """
        self._wait_for_submit_enabled()
        if self.driver.is_tui():
            self.driver.press_key("post-submit-button", "Enter")
        else:
            self.driver.click("post-submit-button")

    def compose_error_text(self) -> str:
        """The composer's own error (``compose-error``), or ``""`` when none shows."""
        if not self.driver.is_visible("compose-error"):
            return ""
        return self.driver.get_text("compose-error") or ""

    def compose_audience(self) -> str:
        """The audience ``compose-gate-tier-select`` shows as picked — Public, a
        tier, a room, or the sale option — as its display label."""
        return self.driver.get_text("compose-gate-tier-select") or ""

    def driver_error_text(self) -> str:
        """Best-effort read of the page's `error-message`, for failure text."""
        try:
            if not self.driver.is_visible("error-message"):
                return ""
            return self.driver.get_text("error-message")
        except Exception:
            return ""

    def _wait_for_submit_enabled(self, timeout: float = SUBMIT_ENABLED_BUDGET_S) -> None:
        """Wait for `post-submit-button` to become clickable, and diagnose a
        timeout rather than leaving Playwright to report a bare
        "element is not enabled".

        Web disables the button on `composing || !composeBody.trim() ||
        !feedReady` — three very different faults (a submit still in flight,
        text that never reached the field, a feed manager that never became
        ready) which are indistinguishable from the DOM. A raw click timeout
        here reads as a product bug in whatever the test was actually about;
        under fleet load the real cause is usually the feed manager still
        booting. Latency-independent per convention 14: a generous ceiling
        polled to a deadline, costing a green run nothing.
        """
        if not self.driver.is_web():
            return
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.is_enabled("post-submit-button"):
                return
            time.sleep(0.2)
        try:
            body = self.compose_body_text()
        except Exception as e:  # noqa: BLE001 — diagnostic only; never mask the timeout
            body = f"<{type(e).__name__}: {e}>"
        raise AssertionError(
            "web: 'post-submit-button' still disabled after "
            f"{timeout:.0f}s — web gates it on "
            "`composing || !composeBody.trim() || !feedReady`. Observed: "
            f"compose-text-field present={self.driver.is_visible('compose-text-field')}, "
            f"compose-body={body!r}, "
            f"compose-file-ready={self.driver.is_visible('compose-file-ready')}, "
            f"feed-view={self.driver.is_visible('feed-view')}, "
            f"error={self.driver_error_text()!r}. Read the THREE terms "
            "separately. An EMPTY compose-body is `!composeBody.trim()` and says "
            "nothing about the manager — suspect the composer being cleared "
            "under the test, e.g. by an earlier submit finishing. A body that "
            "IS present, with an empty error, is `!feedReady` or a submit still "
            "in flight. ⚠ This text used to assert the `!feedReady` reading "
            "outright, and five passes chased a manager "
            "that was fine while the real fault was the previous submit's "
            "success path clearing a composer the test had already typed the "
            "NEXT post into."
            # The minimal repro died at THIS door, not at the post wait:
            # the manager build is the step that never finished, and whether it
            # threw on the way up is visible only in the console ring.
            f"{self._compose_console_tail()}"
        )

    def _settled_post_count(self) -> int:
        """The rendered post count to hand ``_wait_for_new_post`` as its
        baseline — read only once the feed list is authoritative.

        That wait's count fallback asks "did the list grow?", which is only a
        question about OUR post if the baseline was taken from a list that had
        already loaded. On web it often was not: the composer is enabled at
        ``feedReady``, BEFORE the page's ``loadAll()`` runs its four reads and
        the local-feed query, so on a fresh per-test mount a baseline read here
        counted an EMPTY list. The page's own load then landed the posts earlier
        journeys left behind, the count went up, and the wait returned before
        the new post existed. Measured twice in one run:
        ``test_post_detail_opens`` opened the PREVIOUS test's post, and the c2pa
        badge test typed its second post while its first was still uploading.

        The anchor is the feed manager's own reload counter, convention 14's
        causal barrier, never a settle-sleep: ``committed_gen > 0`` is "a feed
        query has landed its verdict on this manager", and web's manager is
        fresh per mount (the per-test ``reset`` is a hard reload), so that is
        this page's first load. A no-op thereafter — a loaded list stays loaded.

        Web only, deliberately: it is where the gap was measured and where the
        counters are served live. The native apps PUSH their state, so the
        same read there needs ``driver.barrier()`` in front of it
        (``helpers.waiting.feed_reload_baseline``) — adopt it per app, with
        that barrier, if one shows the same early return.
        """
        if self.driver.is_web():
            await_feed_reload_after(
                self.driver, 0, budget_s=FEED_FIRST_LOAD_BUDGET_S,
                what="this page's feed manager was built",
            )
        return self.driver.count("feed-post-text")

    def _wait_for_new_post(
        self, initial_count: int, expected_text: str | None = None,
        timeout: float = 20,
    ) -> None:
        """Wait for a newly-composed post to land in the feed.

        Timeout defaults to 20s because some clients reload the entire feed
        after compose (Posts.Clear + LoadPostsAsync on Windows), which can
        exceed 10s once a dozen posts have accumulated in the feed.

        Detection is content-first when ``expected_text`` is given: a new post
        sorts to the top (newest-first), so we wait until the first feed post's
        body contains it. This is robust on a feed that has hit its display cap
        (every app caps at INITIAL_LIMIT=50), where a strictly-growing count
        can NEVER increase — a new post displaces an older one rather than
        adding a row, so a count-only wait would always time out. The
        count-increase check is kept as a small-feed fallback (and the sole
        signal when callers don't have the body).

        Reads via ``post_count()`` / ``first_post_text()`` — the unified
        accessors — so this is the same UI-element read on web/iOS/Linux/Windows
        but a feed-*state* read on compose-state clients (macOS), whose post
        submit + feed reload run asynchronously after ``set_state`` returns
        (see ``create_post``).
        """
        deadline = time.monotonic() + timeout
        top = ""
        while time.monotonic() < deadline:
            if expected_text is not None:
                try:
                    top = self.first_post_text()
                    if expected_text in top:
                        return
                except Exception:
                    pass
            if self.post_count() > initial_count:
                return
            # Check for compose error early (all apps) — a visible compose-error
            # means submit failed; fail fast with its reason instead of timing out.
            if self.driver.is_visible("compose-error"):
                err = self.driver.get_text("compose-error")
                raise RuntimeError(f"Compose failed: {err}")
            time.sleep(0.3)
        # Self-diagnosing timeout (e2e rule 6): the bare "did not appear" hid
        # whether the feed even rendered, the final count vs. the initial, and
        # what actually sits at the top — so report all three (via this action's
        # own state-or-UI accessors, each guarded; a raw element diagnose would
        # misreport on macOS, which reads the feed from state, not feed-post-text).
        # Classification: page not rendered => feed never loaded; final == initial
        # => submit produced no row (a visible compose-error already fail-fasts
        # above, so suspect a silently-dropped submit); final > initial but
        # expected_text absent from top => a *different* post sorted to the top
        # (echo/sort race).
        try:
            final = self.post_count()
        except Exception as e:  # noqa: BLE001 — diagnostic only; never mask the timeout
            final = f"<{type(e).__name__}: {e}>"
        try:
            page_rendered = self.is_visible()
        except Exception as e:  # noqa: BLE001 — diagnostic only
            page_rendered = f"<{type(e).__name__}: {e}>"
        detail = f" expected {expected_text!r} at top;" if expected_text is not None else ""
        raise TimeoutError(
            f"New post did not appear in feed:{detail} feed_page_rendered="
            f"{page_rendered}, initial_count={initial_count}, final_count={final}, "
            f"top_post={top[:80]!r}{self._compose_console_tail()}"
        )

    #: How many trailing browser-console lines a silent-drop timeout carries.
    #: The whole ring is unbounded and mostly boot noise; the drop happens at
    #: submit, so the tail is where its evidence is.
    CONSOLE_TAIL_LINES = 40

    #: Breadcrumb prefixes that are ALWAYS carried, even from outside the tail.
    #:
    #: The tail rule above is right for the failure it was written for — a
    #: submit silently dropped at submit time. It is wrong for the failure where
    #: the composer never became READY at all, because that evidence is written
    #: during BOOT: the launch machine's routing, the feed page's mount, the
    #: actor-scope pass. Those lines are the first thing off the ring and the
    #: last thing a reader can do without them is guess — which is exactly what
    #: a run against a 500-line ring reporting its final 40 lines invites.
    #:
    #: Substring, not `startswith`: the bridge prefixes each line with its
    #: console level. Capped, so a page that loops its boot cannot flood the
    #: message it is supposed to make readable.
    # Lines worth carrying out of the elided head. The four breadcrumb prefixes
    # say what the boot DID; `[pageerror]` is the different and stronger class —
    # what DIED. A `wasm_bindgen_futures` task whose poll throws a JS exception
    # dies mid-poll and its JS promise never settles: no resolution, no
    # rejection, so every `await` on it hangs forever and every deadline inside
    # it is dead too (the bound lived in the killed task). The bridge captures
    # uncaught errors and unhandled promise rejections alike
    # (`web-bridge/server.py`'s `on_pageerror`), and that line is the only
    # witness the failure leaves anywhere — the DOM shows a composer that is
    # merely disabled, and the breadcrumbs show a boot that ran fine. It is
    # emitted when the task is polled, i.e. long before the timeout, so on any
    # page that logs afterwards it falls off the tail exactly as the boot lines
    # do.
    # `[singleton]` is the third class again: not what the boot did, not what
    # died unexpectedly, but a build the SPA itself DECLARED dead. It is what
    # `$lib/singleton-build.ts`'s settle deadline writes when a memoized build
    # neither resolves nor rejects inside its budget, and it names WHICH builder
    # (feed manager / conversations manager / search manager / event drafts /
    # WS-RPC client) — the one line that turns "the composer is disabled and
    # nothing says why" into a located failure. It is written ~45 s before the
    # 90 s compose ceiling, so on a page that logs anything afterwards it falls
    # off the tail exactly as the boot lines do.
    CONSOLE_PROBE_MARKERS = (
        '[launch]', '[feed]', '[actor-scope]', '[succession]', '[pageerror]',
        '[singleton]',
    )
    # Newest-first when capped, so a page erroring in a loop keeps its most
    # recent evidence; the header names how many older ones were dropped.
    CONSOLE_PROBE_MAX = 30

    def _compose_console_tail(self) -> str:
        """Web-only: the tail of the bridge-captured console ring, appended to a
        compose timeout.

        A silently-dropped submit (convention 11's failure mode) leaves NOTHING
        in the DOM — no `compose-error`, no new row — so every DOM-side probe
        above reports the same "final == initial" for a submit that was never
        sent, one that threw inside `handleCompose`, and one that landed but
        never rendered. The console ring is the only surface that separates
        them, and it is already captured for exactly this (`web.py::console_log`,
        which also survives an unclean relaunch). Convention 6: the failure
        carries its own diagnosis rather than requiring a re-run with `-s`.

        Best-effort and never masking: any driver that has no console (every
        native app) or a bridge read that fails contributes nothing.
        """
        try:
            if not self.driver.is_web():
                return ""
            lines = self.driver.console_log()
        except Exception as e:  # noqa: BLE001 — diagnostic only; never mask the timeout
            return f", console=<{type(e).__name__}: {e}>"
        if not lines:
            return ", console=<empty>"
        tail = lines[-self.CONSOLE_TAIL_LINES:]
        elided_lines = lines[: len(lines) - len(tail)]
        elided = len(elided_lines)
        # Rescue the boot breadcrumbs and any uncaught page error from the
        # elided head — see CONSOLE_PROBE_MARKERS. Kept in ring order, capped,
        # and labelled as a rescue so nobody reads them as contiguous with the
        # tail.
        probes = [ln for ln in elided_lines if any(m in ln for m in self.CONSOLE_PROBE_MARKERS)]
        dropped_probes = max(0, len(probes) - self.CONSOLE_PROBE_MAX)
        probes = probes[-self.CONSOLE_PROBE_MAX:]
        head = f"<{elided} earlier line(s) elided>\n" if elided else ""
        if probes:
            more = f" (+{dropped_probes} older elided)" if dropped_probes else ""
            head += (
                f"--- boot breadcrumbs + errors rescued from those {elided}{more} ---\n"
                + "\n".join(probes)
                + "\n--- end rescued ---\n"
            )
        body = "\n".join(tail)
        return f"\n--- browser console (last {len(tail)} of {len(lines)}) ---\n{head}{body}"

    def _use_compose_state(self) -> bool:
        """Use state protocol for compose on clients where compose UI is inaccessible."""
        return self.driver.is_macos()

    def create_post(
        self, text: str, timeout: float = 20, expect_top: str | None = None
    ) -> None:
        """Type text, submit, wait for post to appear.

        ``timeout`` is how long to tolerate the post taking to LAND (passed
        through to ``_wait_for_new_post``); a visible ``compose-error`` still
        fails fast regardless of it. Callers composing right after a nest flip
        pass a generous value so a slow-but-correct reconnect+re-mint+re-fetch
        under machine load is not misread as a failure (see
        ``test_nest_flip_resilience``).

        ``expect_top`` is the text the rendered top card will contain, when that
        is not ``text`` itself. A body carrying markdown renders its own way —
        an image's ``![alt](url)`` source shows as its alt text — and on a feed
        at its display cap the text match is the only signal that can see a new
        post land, so a raw-body match would time out with the post on top."""
        expected = text if expect_top is None else expect_top
        if self._use_compose_state():
            # set_state returns once the compose model is patched; the app then
            # submits + reloads the feed asynchronously. Wait for the post to
            # land (state read) so callers don't race the in-flight reload.
            initial = self.post_count()
            self.driver.set_state({"compose": {"post": {"body": text}}})
            self._wait_for_new_post(initial, expected_text=expected, timeout=timeout)
            return
        self._open_composer()
        initial = self._settled_post_count()
        self.driver.type_text("compose-text-field", text)
        self._wait_for_submit_enabled()
        self.driver.click("post-submit-button")
        self._wait_for_new_post(initial, expected_text=expected, timeout=timeout)

    def create_gated_post(self, full_text: str, preview: str, tier: str,
                          image_path: str | None = None) -> None:
        """Compose a tier-gated post through the composer's gate-to-tier
        controls (`compose-gate-tier-select` + `compose-gate-preview-field`,
        feed.md § Encryption at rest; monetization.md § Pillars 2+3): pick the
        tier, fill the public teaser, type the full body, submit. The list card
        renders the plaintext teaser, so the new-post wait keys on `preview`.

        `image_path` attaches a file through the same `compose-file` door the
        public composer uses — there is no separate gated picker, by design
        (a per-audience composer would be a priority #1 divergence). **The tier
        is selected FIRST and the file attached after**, matching the order the
        app itself must follow: the attachment's seal is decided by the
        composer's audience, so an app that uploads before the audience is known
        publishes a plaintext copy of a restricted post's photo that no blob
        DELETE can remove (`ui/media.md` § Encryption at rest). Driving the two
        steps in this order is what makes a regression to the old ordering show
        up as a red here rather than as a silent leak.
        """
        self._open_composer()
        initial = self._settled_post_count()
        self.driver.select("compose-gate-tier-select", tier)
        self.driver.wait_for("compose-gate-preview-field")
        self.driver.type_text("compose-gate-preview-field", preview)
        self.driver.type_text("compose-text-field", full_text)
        if image_path is not None:
            self._attach_file("compose-file", image_path)
        self._wait_for_submit_enabled()
        self.driver.click("post-submit-button")
        self._wait_for_new_post(initial, expected_text=preview)

    def wait_for_audience_option(self, option: str, timeout_s: float) -> bool:
        """Poll until the composer's audience select (`compose-gate-tier-select`)
        offers ``option`` — e.g. a room the author has just joined, whose option
        appears once the app has re-read its rooms. Selecting it IS the read: an
        agent refuses a value the frame did not paint (`SelectOptionNotOffered`,
        convention 11), and selecting an offered audience is idempotent.

        Returns True as soon as it is offered, else False after ``timeout_s`` so
        the caller asserts and self-diagnoses (convention 6)."""
        from drivers.http_bridge import SelectOptionNotOffered

        self._open_composer()
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            try:
                self.driver.select("compose-gate-tier-select", option)
                return True
            except SelectOptionNotOffered:
                time.sleep(0.5)
        return False

    def sell_post(self, full_text: str, preview: str, price: str,
                  subscribers_get_it_free: bool = True,
                  image_path: str | None = None) -> None:
        """Sell a post through the composer's gate select — "Sell this post…",
        the third answer to `compose-gate-tier-select` (`monetization.md`
        § Per-post pay-to-unlock; IDs user-approved 2026-07-29).

        Unlike `create_gated_post` this needs **no pre-existing tier**: the
        shared `prepare_sell_post` auto-mints a degenerate single-post tier as
        part of the submit, which is why a client without an author-side
        subscriptions surface can still drive the whole journey.

        `subscribers_get_it_free` is the ratified rank knob and **defaults on**
        in the UI, so the toggle is only clicked to turn it OFF — clicking it
        unconditionally would silently assert the opposite of the default.

        `image_path` attaches a file through the same `compose-file` door every
        other audience uses — there is no separate sold-post picker, by design.
        **The sale is selected FIRST and the file attached after**, the same
        order `create_gated_post` drives and for the same reason: a sold post's
        photo seals under the tier the sale mints, so an app that uploads before
        the audience is settled publishes a plaintext copy no blob DELETE can
        remove (`ui/media.md` § Encryption at rest). Selling is the harder case
        — the tier does not exist yet either, which is why the mint is split in
        two (`behavior/monetization.md` § Per-post pay-to-unlock).
        """
        self._open_composer()
        initial = self._settled_post_count()
        self.driver.select("compose-gate-tier-select", S.feed.post.gate_sell)
        self.driver.wait_for("compose-sell-price")
        self.driver.type_text("compose-sell-price", price)
        if not subscribers_get_it_free:
            self.driver.click("compose-sell-subscribers-free")
        self.driver.type_text("compose-gate-preview-field", preview)
        self.driver.type_text("compose-text-field", full_text)
        if image_path is not None:
            self._attach_file("compose-file", image_path)
        self._wait_for_submit_enabled()
        self.driver.click("post-submit-button")
        self._wait_for_new_post(initial, expected_text=preview)

    def gated_badge_count(self) -> int:
        """Number of visible gated-post tier badges (`gated-post-badge`)."""
        return self.driver.count("gated-post-badge")

    def gated_badge_text(self, index: int = 0) -> str:
        """Tier name shown on the index-th gated-post badge. Call
        `wait_for_gated_badge_text` first — see its doc comment for why a bare
        read straight after `sell_post()` races."""
        return self.driver.get_text("gated-post-badge", index=index)

    def wait_for_gated_badge_text(self, index: int = 0, timeout: float = 10.0) -> str | None:
        """Poll until the index-th `gated-post-badge` has a non-empty,
        readable tier name; return it, or `None` at the deadline.

        The deadline-polled twin of `gated_badge_count()` + `gated_badge_text()`
        (convention 14) — mirrors `wait_for_first_post_text`'s doc comment: a
        card sold via `sell_post()` races the feed's re-query and the badge's
        own value render, and the two are not the same race. A bare
        `gated_badge_count() >= 1` can pass (the slot exists) the instant
        before `gated_badge_text()` 404s on it (the value hasn't landed) —
        `count`d present and text-readable are not the same registry moment,
        so polling only the count under-covers this exact class (measured
        2026-08-25: a `LookupError` on `index=0`
        right after `gated_badge_count() >= 1` had just passed). Poll the
        TEXT READ itself, catching the lookup racing the count, exactly as
        `wait_for_first_post_text` catches a mid-render exception while
        polling."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                text = self.gated_badge_text(index)
                if text:
                    return text
            except Exception:  # noqa: BLE001 — mid-render races are expected while polling
                pass
            time.sleep(0.2)
        return None

    def wait_for_unlock_offer_price(
        self, timeout: float = 10.0, *, card: int | None = None
    ) -> bool:
        """Deadline-poll for `gated-post-price` to resolve AND settle (testing.md
        convention 14: a named generous budget, never a fixed sleep) — the
        buyer's price read (monetization.md § Per-post pay-to-unlock → the
        buyer's price read is post-addressed) is an async
        `fauna.subscriptions.post_unlock.get` round trip triggered on render,
        not present in the initial snapshot.

        Requires two consecutive equal, non-empty text reads before returning
        true — the same "counted present and text-readable are not the same
        registry moment" race `wait_for_gated_badge_text` documents above, but
        caught here one step later: on windows the element can already be
        readable (no `LookupError`) while its automation Name still reflects a
        mid-transition value from container recycling — measured
        2026-09-06: `unlock_offer_price_text()`
        read `'e$3'` once (1 of 4 runs) against a seller-typed `'$3'`, with
        the very next run's read clean. A bare `count() >= 1` (the original shape here)
        cannot see this at all, since the element genuinely IS present and
        readable — just not yet settled to its final value. Stability across
        two 200ms-apart reads is the general fix: it needs no assumption about
        what a correct price string looks like, only that a settled one stops
        changing.

        ``card`` scopes the read to one post card (``post-card[card]``) — on a
        populated nest the buyer's feed can hold other sold posts, and the
        first price on screen is then not this test's."""
        deadline = time.monotonic() + timeout
        last: str | None = None
        while time.monotonic() < deadline:
            try:
                text = self.unlock_offer_price_text(card=card)
            except Exception:  # noqa: BLE001 — mid-render races are expected while polling
                text = None
            if text and text == last:
                return True
            last = text
            time.sleep(0.2)
        return False

    def unlock_offer_price_text(self, index: int = 0, *, card: int | None = None) -> str:
        """The resolved buyer's price read shown on the index-th sold post's
        teaser card (`gated-post-price`), or on the card ``card`` names —
        call `wait_for_unlock_offer_price` first."""
        if card is not None:
            return self.driver.get_text("gated-post-price", scope=f"post-card[{card}]")
        return self.driver.get_text("gated-post-price", index=index)

    def buy_unlock_offer(self, index: int = 0, *, card: int | None = None) -> None:
        """Click the index-th sold post's self-serve buy affordance
        (`gated-post-buy-button`) — starts the purchase (the existing
        subscribe flow against the resolved offer's tier) without a claim
        code.

        Fire-and-forget from the UI's own view: the click returns as soon as
        the handler is invoked, while the subscribe round trip is still in
        flight. Pair it with `wait_for_unlock_offer_cleared` before doing
        anything that tears the caller's session down (an actor switch).
        ``card`` aims the click at one post card instead of the index-th
        button on screen.
        """
        if card is not None:
            self.driver.click("gated-post-buy-button", scope=f"post-card[{card}]")
            return
        self.driver.click("gated-post-buy-button", index=index)

    def wait_for_unlock_offer_cleared(
        self, timeout: float = 30.0, *, card: int | None = None
    ) -> bool:
        """Deadline-poll for the buy affordance to un-render — the purchase's
        completion signal (testing.md convention 14: a named generous budget
        polled to a deadline, never a fixed sleep).

        `FeedManager::buy_unlock_offer` clears the post's `unlock_offer` and
        re-notifies on success, precisely so a successful buy un-renders its
        own affordance (nothing left to buy, and re-showing the price would
        invite a duplicate subscribe). The button's render is gated on that
        same field on every app, so its disappearance is the one
        latency-independent state a caller can wait on to know the subscribe
        actually landed.

        ``card`` waits on one post card's button only; without it, every buy
        button on screen must go, which a feed holding someone else's sold
        post never satisfies.
        """
        scope = f"post-card[{card}]" if card is not None else None
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.count("gated-post-buy-button", scope=scope) == 0:
                return True
            time.sleep(0.2)
        return False

    # ── the post tip surface (monetization.md § Tips) ─────────────────────────

    def tip_total_text(self, index: int = 0) -> str:
        """The summed tip amount on the index-th card (`post-tip-total`).

        Present **iff** the resolved total is non-zero — deliberately NOT the
        same condition as `post-tip-count`: a post whose every receipt carried
        an unparseable invoice has real tips and no renderable amount, and § Tips
        forbids showing that as "0 sats". Use `has_tip_total` to assert absence.
        """
        return self.driver.get_text("post-tip-total", index=index)

    def has_tip_total(self, index: int = 0) -> bool:
        """Whether the index-th card shows an amount at all."""
        return not self.driver.is_absent("post-tip-total", scope=f"post-card[{index}]")

    def tip_count_text(self, index: int = 0) -> str:
        """The tip count on the index-th card (`post-tip-count`) — counts EVERY
        tip, including any that reported no amount, so it can render while
        `post-tip-total` does not."""
        return self.driver.get_text("post-tip-count", index=index)

    def open_tip_list(self, index: int = 0) -> None:
        """Open the index-th card's attribution window (`post-tip-list-button`
        → `post-tip-list`)."""
        self.driver.click("post-tip-list-button", index=index)

    def tip_item_count(self) -> int:
        """Rows in the open attribution window (`post-tip-item`).

        Bounded by the nest's window cap, never the post's true tip count — the
        list's own text carries the "and N more" remainder from `has_more`.
        """
        return self.driver.count("post-tip-item")

    def tip_item_text(self, index: int = 0) -> str:
        """The index-th attribution row: who tipped, and how much (or the
        localized "amount not reported" — never "0 sats")."""
        return self.driver.get_text("post-tip-item", index=index)

    def create_post_with_tags(self, text: str, tags: str) -> None:
        """Type text + tags, submit, wait for post to appear."""
        if self._use_compose_state():
            initial = self.post_count()
            tag_list = [t.strip() for t in tags.split(",")]
            self.driver.set_state({"compose": {"post": {"body": text, "tags": tag_list}}})
            self._wait_for_new_post(initial, expected_text=text)
            return
        self._open_composer()
        initial = self._settled_post_count()
        self.driver.type_text("compose-text-field", text)
        if self.driver.is_visible("compose-tags-field"):
            self.driver.clear_and_type("compose-tags-field", tags)
        self._wait_for_submit_enabled()
        self.driver.click("post-submit-button")
        self._wait_for_new_post(initial, expected_text=text)

    def create_post_with_image(self, text: str, image_path: str,
                               before_submit=None) -> None:
        """Type text, attach image, submit, wait for post to appear.

        ``before_submit`` (a zero-arg callable) runs with the picture attached
        and the post not yet sent — the time the user spends writing, for a
        test that acts on the nest meanwhile (a blob sweep)."""
        self._open_composer()
        initial = self._settled_post_count()
        self.driver.type_text("compose-text-field", text)
        self._attach_file("compose-file", image_path)
        if before_submit is not None:
            before_submit()
        self._wait_for_submit_enabled()
        self.driver.click("post-submit-button")
        self._wait_for_new_post(initial, expected_text=text, timeout=20)

    def create_post_with_video(self, text: str, video_path: str) -> None:
        """Type text, attach a video file, submit, wait for post to appear.

        Same upload mechanism as `create_post_with_image` (`compose-file`
        accepts any file; the client renders a video vs. image branch off the
        attachment's own `media_type`) — a separate name so a search for
        "video" finds this vehicle instead of a generic image helper.
        """
        self._open_composer()
        initial = self._settled_post_count()
        self.driver.type_text("compose-text-field", text)
        self._attach_file("compose-file", video_path)
        self._wait_for_submit_enabled()
        self.driver.click("post-submit-button")
        self._wait_for_new_post(initial, expected_text=text, timeout=20)

    def create_post_with_image_and_tags(self, text: str, tags: str,
                                         image_path: str) -> None:
        """Type text + tags, attach image, submit."""
        self._open_composer()
        initial = self._settled_post_count()
        self.driver.type_text("compose-text-field", text)
        if self.driver.is_visible("compose-tags-field"):
            self.driver.clear_and_type("compose-tags-field", tags)
        self._attach_file("compose-file", image_path)
        self._wait_for_submit_enabled()
        self.driver.click("post-submit-button")
        self._wait_for_new_post(initial, expected_text=text, timeout=15)

    # --- Querying posts ---

    def model_post_bodies(self) -> list[str] | None:
        """The post bodies the app's OWN published list carries, or ``None`` when
        this app publishes no such list.

        The element readers above see only what the view actually painted; this
        sees the model the view binds to (windows publishes its bound collection
        via ``AppDataSnapshot.SetFeedPosts``, apple its feed state, linux/web
        their ``data.feed.posts``). Reading both separates the two halves of a
        "the post never showed up" failure that the element read alone conflates:
        absent HERE means the re-query's result never carried it (look upstream —
        fetch, dedupe, filter); present here but absent from the elements means it
        reached the model and the view never painted it (look at the render — a
        virtualized row, a reconcile key, a template).

        ``None`` (not ``[]``) when the app omits the key, so a caller can say "this
        app never told us" instead of reporting a misleading empty list — the
        distinction convention 11 draws for every other capability read.
        """
        state = self.driver.get_state()
        feed = (state or {}).get("data", {}).get("feed")
        if feed is None:
            return None
        return [p.get("body") or "" for p in feed.get("posts", [])]

    def _feed_posts_from_state(self) -> list[dict]:
        """Get feed posts from state protocol. Returns [] if unavailable.

        Drops posts the client marks ``is_muted`` (collapsed behind
        ``feed-post-muted``, not revealed) — mirrors the element-based read on
        every other app, where a collapsed post's ``feed-post-text`` simply
        does not exist (topic-factors.md § Scoring), so ``post_count()`` /
        ``post_text()`` naturally exclude it there too. Filtering centrally
        here keeps every state-backed reader index-aligned with the
        element-backed readers without a per-call-site special case.
        """
        state = self.driver.get_state()
        feed = (state or {}).get("data", {}).get("feed")
        if feed is None:
            return []
        return [p for p in feed.get("posts", []) if not p.get("is_muted")]

    def _use_state_for_feed_reads(self) -> bool:
        """Use state protocol for feed reads where the feed-list element is inaccessible.

        Both apple apps: the in-process `feed-post-text` element never registers
        in the lazy feed list (a deferred TRACK-1 concern), so the harness reads the
        feed from the in-process state push. The state `body` field is the painted
        document plaintext on both — the SAME text every other app's element read
        returns (macOS FaunaMacApp.swift, iOS FaunaApp.swift both serialize
        `renderDocumentToPlaintext(document:)`) — so the state-read
        fallback matches the element read (render-model.md § D6)."""
        return self.driver.is_macos() or self.driver.is_ios()

    def first_post_text(self) -> str:
        if self._use_state_for_feed_reads():
            posts = self._feed_posts_from_state()
            return posts[0].get("body", "") if posts else ""
        return self.driver.get_text("feed-post-text", index=0)

    def post_text(self, index: int) -> str:
        if self._use_state_for_feed_reads():
            posts = self._feed_posts_from_state()
            return posts[index].get("body", "") if index < len(posts) else ""
        return self.driver.get_text("feed-post-text", index=index)

    # ── Test-state injection (tier_2: feed_inject_posts) ──────────────────────

    def inject_posts(self, posts: list[dict]) -> None:
        """Seed the feed post list via the ``feed_inject_posts`` test-state command.

        Replaces the live feed snapshot with a ``Loaded`` list built from ``posts``
        — each a ``fauna_feed::test_support::TestPostSpec`` shape: ``{"post_id",
        "author", "body"?, "verification"?, "timestamp"?, "source"?, "tags"?,
        "has_media"?, "is_reply"?}``. ``verification`` is the plain
        ``VerificationStatus`` variant string (``"Unchecked"`` / ``"Verified"`` /
        ``"Failed"``); the unverified-source badge renders **iff** ``"Failed"``.

        The client renders each post's document from its ``body`` in Rust, so this
        sends only plain specs (web: ``WasmFeedManager.injectPostsForTest`` via the
        ``__fauna_callCommand`` bridge; linux: ``handle_feed_inject_posts`` on the
        test agent). The only way to drive a ``Failed`` post — a real signed post
        the nest serves is only ever ``Unchecked``/``Verified`` (the manager flips
        ``verification`` to ``Failed`` only where THIS client's own envelope
        verification fails). Order is preserved (the nest supplies feed order; the
        client never re-sorts), so ``posts[i]`` renders as ``post-card[i]``.
        """
        self.driver.call_command("feed_inject_posts", {"posts": posts})

    def seed_cue_rollup_for_test(self, content_ids: list[str]) -> None:
        """Seed the live engagement-cue engine with `content_ids` (each recorded
        a `watch-complete` verdict) via the ``feed_seed_cue_rollup_for_test``
        test-state command, and — unlike ``inject_posts`` above — PUT the
        sealed rollup to the nest for real. Gives a capture-less client (web,
        windows) an actual ``cues:v1`` row for "Clear activity data" to delete,
        so the clear-button assertion can check the row is gone rather than
        merely that the click didn't error. tui/linux: `feed_seed_cue_rollup_for_test`
        on the test agent; web: `WasmFeedManager.setCueRollupForTest`; windows:
        `FfiFeedManager.SetCueRollupForTest`. See
        ``fauna_feed::FeedManager::set_cue_rollup_for_test``.
        """
        self.driver.call_command(
            "feed_seed_cue_rollup_for_test", {"content_ids": content_ids}
        )

    def inject_error_for_test(self, message: str, key: str = "feed.error_load") -> None:
        """Drive the feed page's ``error-message`` directly via ``feed_inject_error``.

        There is no *product* path that fails a feed fetch on demand (a real
        failure needs the nest's own query to error), so this is the feed twin
        of ``ConversationsActions.inject_send_failure_for_test``
        (``FeedManager::inject_error_for_test``; tui
        wired 2026-08-21).
        """
        self.driver.call_command("feed_inject_error", {"key": key, "message": message})

    def hold_next_reload_for_test(self) -> None:
        """Arm the feed manager's one-shot reload hold
        (``FeedManager::hold_next_reload_for_test``): the NEXT reload still
        publishes the list it decided to keep or clear, then parks before its
        fetch until :meth:`release_held_reload_for_test`. That parked moment is
        what "until the new page lands" names, and nothing else can hold it
        open without a clock.

        While a hold is armed the app's agent starts a feed op without awaiting
        it — a held reload cannot land until released, so an awaited trigger
        would park the agent with it. So the ordinary triggers (re-entering the
        Feed page, typing a search) return at once, and the test reads the
        screen while the reload is parked.
        """
        self.driver.call_command("feed_hold_next_reload", {})

    def release_held_reload_for_test(self) -> None:
        """Release the reload :meth:`hold_next_reload_for_test` parked; it then
        fetches and commits as usual. A no-op when nothing is held."""
        self.driver.call_command("feed_release_held_reload", {})

    def reload_in_flight(self) -> bool:
        """Whether a feed reload has started and not yet committed — read
        behind a barrier, so it cannot answer from a snapshot older than the
        caller's last action. ``started > committed_gen``: generations are
        claimed at a reload's first statement and stamped at its commit."""
        self.driver.barrier()
        now = feed_reloads(self.driver)
        if now is None:
            raise AssertionError(
                f"{type(self.driver).__name__} does not publish "
                "'feed_reloads' — the in-flight read this needs"
            )
        started, _completed, committed_gen = now
        return started > committed_gen

    def post_card_count(self) -> int:
        """Number of rendered ``post-card`` containers (the badge-scope element)."""
        return self.driver.count("post-card")

    # ── engagement-cue dwell (engagement-cues.md § Cue vocabulary) ────────

    def dwell_on_post(self, index: int, seconds: float) -> None:
        """HONEST dwell: center ``post-card[index]`` in the real viewport via
        the targeted scroll (the client's cue observer samples that same
        scroll position), then hold it there for ``seconds`` of wall clock.
        No injected observation, no clock shortcut — a non-media
        ``watch-complete`` needs ``seconds >= CUE_DWELL_LONG_MS/1000`` (8s)."""
        self.driver.scroll_to("post-card", index=index)
        time.sleep(seconds)

    def scroll_post_into_view(self, index: int) -> None:
        """Center ``post-card[index]``. Dwelling on one card ends when another
        far-away card is centered — the observer emits the accumulated
        exposure when the first card leaves the viewport."""
        self.driver.scroll_to("post-card", index=index)

    def quote_button_count(self) -> int:
        """Number of rendered ``feed-quote-button`` controls — one per post-card's
        interaction bar. Quote is universal across all seven apps (feed.md
        § Interaction bar, ratified 2026-06-27); the button fires
        ``fauna.posts.interact`` action ``quote``. Counting them gates its
        cross-app presence (the Phase-2 interaction-count fan-out)."""
        return self.driver.count("feed-quote-button")

    def wait_post_count(self, expected: int, timeout: float = 5.0) -> int:
        """Wait for the rendered ``post-card`` count to reach ``expected`` (the
        inject→observer→repaint is synchronous on linux and a microtask on web)."""
        deadline = time.monotonic() + timeout
        last = self.post_card_count()
        while time.monotonic() < deadline:
            last = self.post_card_count()
            if last == expected:
                return last
            time.sleep(0.2)
        return last

    def seed_posts(self, posts: list[dict], timeout: float = 20.0) -> int:
        """Navigate to the Feed page and seed ``posts`` via the inject seam,
        re-injecting until the rendered ``post-card`` count matches **and holds**.

        The Feed page re-pulls the (empty) nest feed every time it becomes visible —
        an async reload (linux ``FeedView`` ``connect_map``; the web route's onMount
        ``refreshFeeds``/``selectFeed``) that can land *after* an inject and clear it.
        Rather than guess a settle delay, re-inject until the count sticks: the reload
        is **one-shot per navigation**, so once it has completed the injection holds.
        Cross-app (no per-platform branch in the test); returns the final count.
        """
        expected = len(posts)
        self.driver.navigate_to("feed")
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.inject_posts(posts)
            if self.wait_post_count(expected, timeout=1.5) == expected:
                # Confirm it holds — a late one-shot reload would drop it back to 0.
                time.sleep(0.5)
                if self.post_card_count() == expected:
                    return expected
        return self.post_card_count()

    def post_count(self) -> int:
        if self._use_state_for_feed_reads():
            return len(self._feed_posts_from_state())
        return self.driver.count("feed-post-text")

    def post_author(self, index: int) -> str:
        """The name card ``index`` paints for its author (``post-author``).

        On a state-read app that is the state's ``author_label`` — the string
        the card paints, from the same shared resolver — never the raw
        ``author`` id, which the painted name stopped being once names went
        through the private overlay (`contacts.md` § The private overlay →
        *Where the nickname paints*)."""
        if self._use_state_for_feed_reads():
            posts = self._feed_posts_from_state()
            if index >= len(posts):
                return ""
            return posts[index].get("author_label") or posts[index].get("author", "")
        return self.driver.get_text("post-author", index=index)

    # The `interaction-bar` component's four buttons (feed.md § Interaction bar).
    INTERACTION_BUTTONS = (
        "feed-like-button",
        "feed-reply-button",
        "feed-repost-button",
        "feed-quote-button",
    )

    def interaction_button_label(self, test_id: str, index: int = 0) -> str:
        """The visible label of one interaction button on the post card at
        ``index`` — icon plus count, with the count hidden at 0 (feed.md
        § Interaction bar). Scoped to ``post-card[index]`` for the reason
        :meth:`repost_post` gives: a repost row paints no bar, so a bare button
        index is not a card index."""
        return self.driver.get_text(test_id, scope=f"post-card[{index}]") or ""

    def quoted_post_text(self, index: int = 0) -> str | None:
        """The ``quoted-post`` embed's text on the post card at ``index``, or
        ``None`` when the card paints no embed at all.

        "Paints no embed" is a question about the card's tree, so it is asked
        with a count, never ``is_visible``: windows reads visibility as UIA
        ``IsOffscreen``, so a card an earlier test scrolled out of the viewport
        read as embed-less for as long as a poll cared to wait."""
        scope = f"post-card[{index}]"
        if self.driver.count("quoted-post", scope=scope) == 0:
            return None
        return self.driver.get_text("quoted-post", scope=scope) or ""

    def interaction_button_count(self, test_id: str) -> int:
        """How many post cards expose the interaction button `test_id` (one of
        feed-{reply,repost,quote,like}-button). The interaction bar renders on
        every post-card (feed.md § Interaction bar), so this is ≥ the post count
        once the feed has rendered."""
        return self.driver.count(test_id)

    def like_post(self, index: int = 0) -> None:
        """Tap ``feed-like-button`` on the post card at ``index`` — the
        ``interaction-bar`` component's like verb (``fauna.posts.interact``
        action ``like``, feed.md § User actions).

        The nest's like counter is **idempotent per (actor, post)**: a repeat tap
        by the same actor is a counter no-op (``interact_routes.rs`` — the stable
        ``compute_toggle_event_id`` key), so a caller may not assume N taps move
        the count by N.
        """
        self.driver.click("feed-like-button", index=index)

    def quote_post(self, index: int = 0) -> None:
        """Tap ``feed-quote-button`` on the post card at ``index`` — a direct
        quote-repost with empty commentary (feed.md § Interaction bar).

        Unlike :meth:`like_post` this **composes a post** carrying
        ``Reference::Quote`` (``FeedManager::quote``); the target's
        ``quote_count`` moves nest-side when that post lands, not on the tap
        itself. It is therefore **not** idempotent per (actor, post) the way a
        like is — N taps create N quote posts and move the count by N.
        """
        self.driver.click("feed-quote-button", index=index)

    def reply_post(self, text: str, index: int = 0) -> None:
        """Tap ``feed-reply-button`` on the post card at ``index``, then fill
        + submit the reply compose surface it opens (``feed-reply-dialog`` →
        ``feed-reply-text-field`` → ``feed-reply-submit-button``).

        Like :meth:`quote_post` this **composes a post** (``FeedManager::reply``
        carrying ``Reference::Reply``), never ``fauna.posts.interact`` — whose
        native arm discards the typed text outright (feed.md § Implementation
        status today). Unlike quote, reply needs non-empty text: the submit
        button stays disabled/no-op on blank input.
        """
        self.driver.click("feed-reply-button", index=index)
        # The click opens feed-reply-dialog, which carries the text field —
        # convention 14: wait_for the field, not a sleep (the finding
        # generalizes here).
        self.driver.wait_for("feed-reply-text-field", timeout=10.0)
        self.driver.fill("feed-reply-text-field", text)
        self.driver.click("feed-reply-submit-button")

    def repost_post(self, index: int = 0) -> None:
        """Tap ``feed-repost-button`` on the post card at ``index`` — the repost
        TOGGLE (feed.md § Interaction bar → Repost, ratified 2026-08-10).

        Off → on composes the caller's empty-body ``Reference::Repost`` post
        (``FeedManager::repost``); on → off un-reposts it (``unrepost`` against
        the caller's own repost post, whose id the ``viewer_repost_id``
        projection carries). Unlike :meth:`quote_post`, N taps do NOT create N
        posts — consecutive taps alternate the toggle.

        Scoped to ``post-card[index]`` rather than a bare button index: a
        REPOST ROW renders no interaction bar at all, so in any feed containing
        one the Nth ``feed-repost-button`` is NOT on the Nth card, and a bare
        ``index=`` click lands on the wrong post.
        """
        self.driver.click("feed-repost-button", scope=f"post-card[{index}]")

    def repost_attribution_count(self, index: int = 0) -> int:
        """How many ``repost-attribution`` markers the post card at ``index``
        paints — 1 on a REPOST ROW, 0 on any ordinary post (feed.md
        § Interaction bar → Repost; id user-approved 2026-08-11).

        Scoped to ``post-card[index]`` rather than counted globally: the whole
        point of the element is telling ONE card's kind apart from its
        neighbour's, and a repost row always shares the window with posts that
        must not carry it.
        """
        return self.driver.count("repost-attribution", scope=f"post-card[{index}]")

    def post_state_by_id(self, post_id: str) -> dict | None:
        """The feed-snapshot state row whose ``post_id`` matches — the
        unambiguous read once a repost row shares the window with its original
        (the embed fold makes the ORIGINAL's text appear in the repost row's
        plaintext, so text-keyed reads can match either row). ``None`` if the
        window doesn't hold the post."""
        for p in self._feed_posts_from_state():
            if p.get("post_id") == post_id:
                return p
        return None

    def post_index_by_id(self, post_id: str) -> int:
        """The card index of the post ``post_id`` names, or -1 — the filtered
        state list is index-aligned with the rendered cards (the
        ``_feed_posts_from_state`` contract), so this is the id-keyed way to
        aim a scoped card click when text-keyed indexing is ambiguous."""
        for i, p in enumerate(self._feed_posts_from_state()):
            if p.get("post_id") == post_id:
                return i
        return -1

    def like_landing_diagnosis(self, index: int, post_id: str) -> str:
        """Failure-path only (e2e rule 6): where a like aimed at card ``index``
        for ``post_id`` actually went.

        ``post_index_by_id`` reads the state list and the click addresses the
        rendered cards, and the two agree only while the render has caught up
        with the state. linux rebuilds its rows on every notify. So a like that
        landed on ANOTHER card shows here as some other row liked, and the card
        at ``index`` carries another post's identity. A like recorded nowhere
        means the tap never reached the nest, or the state never re-read it.
        """
        parts = [f"clicked card {index} for post {post_id[:12]}"]
        for attr in ("post", "post-id"):  # linux / web card identity attrs
            try:
                card = self.driver.get_attr("post-card", attr, index=index)
            except Exception as e:  # noqa: BLE001 — diagnostic only
                card = f"<{type(e).__name__}>"
            parts.append(f"card {index} {attr}={card!r}")
        try:
            liked = [
                (i, (p.get("post_id") or "")[:12], (p.get("body") or "")[:40], p.get("like_count"))
                for i, p in enumerate(self._feed_posts_from_state())
                if p.get("viewer_liked")
            ]
            parts.append(f"state rows the viewer has liked (index, id, body, count)={liked}")
        except Exception as e:  # noqa: BLE001 — diagnostic only
            parts.append(f"state rows unreadable: {type(e).__name__}: {e}")
        return "; ".join(parts)

    def post_state_by_text(self, post_text: str) -> dict | None:
        """The first feed-snapshot state row whose body contains ``post_text``
        (the full row dict, incl. ``post_id`` and the repost carrier + viewer
        pair — feed.md § Interaction bar → Repost). Prefer
        :meth:`post_state_by_id` once a repost row may share the window with
        its original. ``None`` if no row matches."""
        for p in self._feed_posts_from_state():
            if post_text in p.get("body", ""):
                return p
        return None

    def wait_for_post_state_by_text(self, post_text: str, timeout: float = 15.0) -> dict | None:
        """Deadline-poll :meth:`post_state_by_text` until it finds a row, not a
        single read: ``create_post``'s own completion criterion is a UI-element
        wait (``first_post_text``), which can settle a tick before the SEPARATE
        state-protocol dump reflects the same post under load — a single-shot
        read right after ``create_post`` races that gap (e2e-conventions.md
        § point 14). Returns ``None`` at the deadline if the post never landed
        in state."""
        deadline = time.monotonic() + timeout
        while True:
            row = self.post_state_by_text(post_text)
            if row is not None:
                return row
            if time.monotonic() >= deadline:
                return None
            time.sleep(0.2)

    def repost_row_state_by_target(self, target_post_id: str) -> dict | None:
        """The state row of a REPOST ROW naming ``target_post_id`` as its
        original (``reposted_post_id`` — the render carrier), or ``None``.
        The state read is the repost-row assertion surface until the rule-A
        ``repost-attribution`` id is approved."""
        for p in self._feed_posts_from_state():
            if p.get("reposted_post_id") == target_post_id:
                return p
        return None

    def wait_for_repost_row_by_target(
        self, target_post_id: str, timeout: float = 10.0
    ) -> dict | None:
        """Deadline-poll for a REPOST ROW naming ``target_post_id``, the
        polling twin of :meth:`repost_row_state_by_target` (e2e-conventions.md
        § point 14). A reload's full post list (marker + repost row + original)
        can land on the state-protocol bridge a beat after the UI/element
        layer already reflects it — measured on windows, where the marker's
        own compose-submit UI wait is satisfied by the element tree before the
        state bridge's snapshot push commits — so a single-shot read right
        after a reload can race a state push still in flight even though the
        row is correct once it lands."""
        deadline = time.monotonic() + timeout
        while True:
            row = self.repost_row_state_by_target(target_post_id)
            if row is not None:
                return row
            if time.monotonic() >= deadline:
                return None
            time.sleep(0.2)

    def wait_for_interaction_count_by_id(
        self, post_id: str, kind: str, expected: int, timeout: float = 30.0
    ) -> int | None:
        """Deadline-poll one interaction count on the post ``post_id`` names —
        the id-keyed twin of :meth:`wait_for_interaction_count`, for windows
        where a repost row's folded embed makes text-keyed reads ambiguous
        (its own counts are 0, so a text-keyed poll for 0 could return before
        the original's counter actually reversed)."""
        deadline = time.monotonic() + timeout
        last: int | None = None
        while True:
            row = self.post_state_by_id(post_id)
            last = row.get(f"{kind}_count") if row else None
            if last == expected:
                return last
            if time.monotonic() >= deadline:
                return last
            time.sleep(0.2)

    def wait_for_interaction_count(
        self, post_text: str, kind: str, expected: int, timeout: float = 30.0
    ) -> int | None:
        """Deadline-poll one of a post's four interaction counts until it reaches
        ``expected``, returning the last value read (``None`` if the post never
        matched).

        A deadline poll, never a settle-sleep (e2e-conventions.md § point 14): a
        green run returns the instant the count lands, so the generous ceiling
        costs nothing and survives a loaded box.
        """
        deadline = time.monotonic() + timeout
        last: int | None = None
        while True:
            counts = self.post_interaction_counts_by_text(post_text)
            last = counts.get(kind) if counts else None
            if last == expected:
                return last
            if time.monotonic() >= deadline:
                return last
            time.sleep(0.2)

    def post_interaction_counts_by_text(self, post_text: str) -> dict | None:
        """Return the four interaction counts ``{like, reply, repost, quote}`` for
        the post whose body contains ``post_text``, read from the feed-snapshot
        state (``data.feed.posts[].{like,reply,repost,quote}_count`` — the shared
        ``PostSummary`` counts, feed.md § Interaction bar, ratified 2026-06-27).

        State-read (not element-read): the counts live on the snapshot post each
        app serializes, the same source :meth:`first_post_text` uses on the
        state-read clients. The cross-app element-read path (a rendered count on
        each ``feed-*-button``) is a follow-on for the apps whose feed list
        registers its row elements in-process. Returns ``None`` if no post matches
        (so a caller distinguishes "no such post" from "counts all zero").
        """
        for p in self._feed_posts_from_state():
            if post_text in p.get("body", ""):
                return {
                    "like": p.get("like_count"),
                    "reply": p.get("reply_count"),
                    "repost": p.get("repost_count"),
                    "quote": p.get("quote_count"),
                }
        return None

    # ── Post detail (ui.yaml feed transition `click post-card → post_detail`) ──

    def open_post_detail(self, index: int = 0) -> None:
        """Open the post-detail dialog for the post-card at `index`."""
        self.driver.click("post-card", index=index)

    def open_compose_dialog(self) -> None:
        """Open the rich compose dialog (ui.yaml `feed-compose-dialog`) via
        `compose-dialog-button`. Reuses `_open_composer` to reach the compose
        bar first — required on iOS (compose-dialog-button only renders once a
        feed is selected); a no-op wait on clients with an always-inline
        composer (web, linux, macos)."""
        self._open_composer()
        self.driver.click("compose-dialog-button")

    def compose_dialog_visible(self) -> bool:
        return self.driver.is_visible("feed-compose-dialog")

    def attach_affordance_state(self) -> dict:
        """Whether the composer offers a usable `compose-file` attach affordance.

        Reaches the compose bar first via `_open_composer` (the platform
        branching belongs here, not in a test file — rule 7). Returns both
        signals so a failure reports which half broke.

        Deliberately does NOT click: the affordance opens an OS-owned file
        panel no in-process agent can drive (`set_input_files`'s own docstring
        — "Native bridges (AT-SPI, FlaUI, Apple) can't control file picker
        dialogs"), which is why the e2e attach path is the `compose.file`
        state-injection command instead. What regresses in practice is the
        affordance being absent or disabled, and that is exactly what this
        reads.
        """
        self._open_composer()
        return {
            "visible": self.driver.is_visible("compose-file"),
            "enabled": self.driver.is_enabled("compose-file"),
        }

    def reveal_remote_content(self, index: int = 0) -> None:
        """Click the post-card's ``load-remote-content-button`` to reveal its blocked
        remote content (render-model.md § D3/D4) — body remote images AND the link
        preview's og:image, which share the one per-post reveal. Scoped to the card at
        ``index`` so a multi-post feed reveals exactly one post."""
        self.driver.click(
            "load-remote-content-button", scope=f"post-card[{index}]"
        )

    def post_detail_visible(self) -> bool:
        return self.driver.is_visible("feed-post-detail-dialog")

    def post_detail_author(self) -> str:
        return self.driver.get_text("feed-post-detail-author")

    def post_detail_body(self) -> str:
        return self.driver.get_text("feed-post-detail-body")

    def post_index_by_text(self, text: str) -> int:
        """The index of the post card containing ``text``, or -1 if absent.

        The public form of ``_find_post_index_by_text``, for tests that assert on
        the RELATIVE order of posts they created rather than on absolute indices.
        Absolute indices only hold on an empty feed, and the e2e ``test_user`` is
        session-scoped — so posts accumulate across a file and an absolute-index
        assertion silently becomes a test-order dependency."""
        return self._find_post_index_by_text(text)

    def _find_post_index_by_text(self, text: str) -> int:
        """Find the index of a post card containing the given text."""
        count = self.driver.count("post-card")
        for i in range(count):
            card_text = self.driver.get_text("post-card", index=i)
            if text in card_text:
                return i
        return -1

    def tag_chip_count(self) -> int:
        """Return the current number of tag-chip elements visible on the page."""
        if self._use_state_for_feed_reads():
            posts = self._feed_posts_from_state()
            return sum(len(p.get("tags", [])) for p in posts)
        return self.driver.count("tag-chip")

    def post_tags_from_state(self, body_text: str) -> list[str]:
        """Find post by body text in state and return its tags.

        More reliable than scoped UI element queries because it doesn't
        depend on platform-specific container accessibility. Works on all
        6 platforms via the state protocol.

        Tags are returned with '#' prefix to match UI rendering convention
        (e.g., '#rust' not 'rust'), consistent with the UI element fallback
        path which reads from tag-chip elements that display '#'-prefixed text.

        Returns [] if no matching post found or client hasn't implemented
        feed serialization.
        """
        state = self.driver.get_state()
        feed = (state or {}).get("data", {}).get("feed")
        if feed is None:
            return []
        for post in feed.get("posts", []):
            if body_text in post.get("body", ""):
                raw_tags = post.get("tags", [])
                return [f"#{t}" if not t.startswith("#") else t for t in raw_tags]
        return []

    def post_tags_by_text(self, post_text: str, timeout: float = 5) -> list[str]:
        """Get tag texts for the post containing the given text.

        Tries state protocol first (works on all platforms), falls back to
        scoped UI element queries if state doesn't have tags. Tags can decode
        asynchronously after the post itself becomes visible/queryable (e.g.
        web's BARE body decode), so this polls up to `timeout` rather than
        reading once — mirrors `post_has_image_by_text`/
        `post_image_blob_hash_by_text`, which already poll for the same
        class of lag.
        """
        deadline = time.monotonic() + timeout
        while True:
            # Try state protocol first — reliable on all platforms
            tags = self.post_tags_from_state(post_text)
            if tags:
                # State returns raw tags; UI renders with # prefix.
                # Normalize to # prefix for consistency with UI path.
                return [t if t.startswith("#") else f"#{t}" for t in tags]
            # Fallback to UI elements (may fail on some platforms)
            idx = self._find_post_index_by_text(post_text)
            if idx >= 0:
                scope = f"post-card[{idx}]"
                tag_count = self.driver.count("tag-chip", scope=scope)
                if tag_count:
                    return [
                        self.driver.get_text("tag-chip", index=i, scope=scope)
                        for i in range(tag_count)
                    ]
            if time.monotonic() >= deadline:
                return []
            time.sleep(0.25)

    def post_tags_since(self, previous_count: int) -> list[str]:
        """Return tag-chip texts that appeared after ``previous_count``.

        Use together with :meth:`tag_chip_count` to isolate tags from a single
        post when multiple tagged posts are visible.
        """
        if self._use_state_for_feed_reads():
            posts = self._feed_posts_from_state()
            all_tags: list[str] = []
            for p in posts:
                for t in p.get("tags", []):
                    all_tags.append(f"#{t}" if not t.startswith("#") else t)
            return all_tags[previous_count:]
        total = self.driver.count("tag-chip")
        return [
            self.driver.get_text("tag-chip", index=i)
            for i in range(previous_count, total)
        ]

    def post_has_image_from_state(self, post_text: str) -> bool | None:
        """`has_media` for the post whose body contains `post_text`, from state.

        Returns `None` when state doesn't answer — the client hasn't
        implemented feed serialization, or this text hasn't shown up there
        yet — so the caller falls back to a scoped element query rather than
        reading a stale "no" as a real answer. Unlike `post_tags_from_state`
        (where an empty list is a harmless signal to fall back either way), a
        presence check needs to tell "found, no image" apart from "no answer
        yet", so this returns the found post's `has_media` even when it is
        `False`.
        """
        state = self.driver.get_state()
        feed = (state or {}).get("data", {}).get("feed")
        if feed is None:
            return None
        for post in feed.get("posts", []):
            if post_text in post.get("body", ""):
                return bool(post.get("has_media"))
        return None

    def post_has_image_by_text(self, post_text: str, timeout: float = 10) -> bool:
        """Whether the post whose body contains `post_text` has rendered its
        image — scoped to THAT post, never a global `post-image` count
        (`e2e-conventions.md` convention 1: "use scoped queries ... rather
        than global counts"). On a feed already holding another image post, a
        global count answers `True` before this post exists, and would keep
        answering `True` even if this post rendered no image at all — the
        generalization is "an assertion that cannot fail is not coverage"
        (`e2e-conventions.md:57`).

        State protocol first (mirrors `post_tags_by_text`), a
        `post-card[i]`-scoped element query as the fallback for a client
        whose feed state doesn't answer (state unimplemented, or this text
        not yet decoded there). apple's cards are scope containers like
        everyone else's (`.automationScope(Ids.postCard, index:)` on both
        apple lists) — it is `feed-post-text`, not the card scope, that its
        lazy list leaves unregistered (`_use_state_for_feed_reads`).
        """
        deadline = time.monotonic() + timeout
        while True:
            from_state = self.post_has_image_from_state(post_text)
            if from_state is not None:
                return from_state
            idx = self._find_post_index_by_text(post_text)
            if idx >= 0 and self.driver.count("post-image", scope=f"post-card[{idx}]") > 0:
                return True
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.25)

    def painted_doc_remote_image_count(self, index: int = 0) -> int | None:
        """Number of ``doc-remote-image`` elements on post-card ``index`` that have
        PAINTED their fetched bytes — a **revealed** body remote image whose url
        loaded and rasterized (``render-model.md`` § D3). The tui half of the
        ``post-image`` paint read (:meth:`_post_image_painted_in`), scoped to one
        card.

        The element registers in every state under the same id, so this counts the
        ``▀`` in its text rather than its presence: a blocked image and a revealed
        one that has not loaded yet both register, and neither has painted. That is
        exactly the distinction the reveal gate is about, and counting elements
        instead of glyphs would erase it.

        Returns ``None`` off tui — the GUI apps paint into a native image view with
        no DOM to inspect (they prove the same path with per-app unit tests), so the
        caller skips the paint assertion and keeps the reveal-gate ones."""
        if not self.driver.is_tui():
            return None
        # HALF_BLOCK — apps/fauna-tui/src/thumbnail.rs. One body can carry several
        # remote images and one reveal frees them all, so walk every instance
        # inside the card rather than reading only the first.
        scope = f"post-card[{index}]"
        return sum(
            1
            for t in self.driver.get_texts("doc-remote-image", scope=scope)
            if "▀" in (t or "")
        )

    def doc_remote_image_painted(self, index: int = 0, timeout: float = 20) -> bool | None:
        """Poll until a ``doc-remote-image`` on post-card ``index`` has painted, or
        the timeout elapses. Returns ``None`` on apps that cannot observe the paint.

        The paint lags the reveal click: the reveal re-emits the document, the app
        then fetches the url off-thread and rasterizes it, and only the tick after
        that repaints. The budget is generous on purpose and costs a green run
        nothing — it polls to a deadline rather than sleeping (testing.md
        convention 14)."""
        if not self.driver.is_tui():
            return None
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if (self.painted_doc_remote_image_count(index) or 0) >= 1:
                return True
            time.sleep(0.5)
        return False

    # ── post-image paint: the last link of fetch → open → decode → paint ─────

    def post_image_painted_by_text(self, post_text: str, timeout: float = IMAGE_PAINT_S) -> bool:
        """Poll until THE post card whose text contains ``post_text`` has PAINTED
        its ``post-image`` — decoded bytes on screen, not the placeholder standing in
        for them — or the timeout elapses.

        Scoped to that card, never "a painted image somewhere in the feed": on a
        feed already holding another image post, a feed-wide read answers ``True``
        before this post exists and keeps answering it if this post never paints
        (the trap ``post_has_image_by_text`` documents; convention 1).

        The paint lags element visibility: ``post_has_image_by_text`` returns once
        the element registers, but the bytes fetch asynchronously (``GET
        /api/v1/blob/<hash>`` → open when sealed → decode → repaint), so this polls.
        An app with no headless paint read declares that instead of answering
        (:meth:`_require_post_image_paint_read`)."""
        return self._wait_post_image_painted(
            lambda: self._post_card_scope_by_text(post_text), timeout,
        )

    def post_detail_image_painted(self, timeout: float = IMAGE_PAINT_S) -> bool:
        """Poll until the open post detail's ``post-image`` has PAINTED, or the
        timeout elapses — the detail twin of :meth:`post_image_painted_by_text`.
        The detail is where a gated post unlocks, so it is the first surface its
        opened photo can reach."""
        return self._wait_post_image_painted(lambda: "feed-post-detail-dialog", timeout)

    def _post_card_scope_by_text(self, post_text: str) -> str | None:
        idx = self._find_post_index_by_text(post_text)
        return f"post-card[{idx}]" if idx >= 0 else None

    def _wait_post_image_painted(self, scope_of, timeout: float) -> bool:
        # The scope is re-resolved every pass: the card can register, or move,
        # while its bytes are still in flight.
        self._require_post_image_paint_read()
        deadline = time.monotonic() + timeout
        while True:
            scope = scope_of()
            if scope is not None and self._post_image_painted_in(scope):
                return True
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.5)

    def _require_post_image_paint_read(self) -> None:
        """Declare an app that cannot yet read whether a ``post-image`` painted.

        Convention 7: the ``None`` this helper used to return off tui, which every
        caller quietly skipped on, reported a paint nobody had observed as covered.
        Called before any polling, so a declared app pays no timeout."""
        if (
            self.driver.is_tui()
            or self.driver.is_linux()
            or self.driver.is_web()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_windows()
        ):
            return
        from helpers.app_surface import skip_unbuilt

        skip_unbuilt(
            self.driver,
            surface="post-image paint state",
            detail=(
                "its test agent answers no get_attr(post-image, state) of "
                "painted / placeholder, so whether a post's fetched bytes opened, "
                "decoded and reached the picture cannot be read headlessly"
            ),
            tracked="docs/goal/behavior/archive-import.md § Implementation status today",
        )

    def post_image_paint_report(self) -> str:
        """Every post card's text and its ``post-image`` paint states, then the open
        detail's — what a failed paint assertion prints (convention 6), so the
        failure says whether the card was missing, still the teaser, carried no
        image, or carried one that never painted. Never raises: it only runs inside
        a failure message."""
        d = self.driver
        parts = []
        try:
            cards = d.count("post-card")
        except Exception as e:  # noqa: BLE001 — a diagnostic must not mask the failure
            return f"<post-card count unreadable: {e}>"
        for i in range(cards):
            try:
                text = (d.get_text("post-card", index=i) or "")[:80]
                parts.append(f"post-card[{i}] {text!r}: {self._post_image_states_in(f'post-card[{i}]')}")
            except Exception as e:  # noqa: BLE001
                parts.append(f"post-card[{i}]: <unreadable: {e}>")
        try:
            parts.append(f"detail: {self._post_image_states_in('feed-post-detail-dialog')}")
        except Exception as e:  # noqa: BLE001
            parts.append(f"detail: <unreadable: {e}>")
        return "; ".join(parts) or "no post-card at all"

    def _post_image_painted_in(self, scope: str) -> bool:
        return "painted" in self._post_image_states_in(scope)

    def _post_image_states_in(self, scope: str) -> list[str]:
        """``painted`` / ``placeholder`` for each ``post-image`` inside ``scope``.

        Each app answers from what actually holds the picture:

        * **tui** paints half-block art, so the picture IS the element's text — a
          ``▀`` per cell (``apps/fauna-tui/src/thumbnail.rs`` HALF_BLOCK). The hash
          / ``□`` placeholder a loading, failed or unopened image paints carries
          none, which is why the glyph is counted rather than "text is non-empty".
        * **linux** asks ``get_attr(post-image, state)``, which its agent answers
          off the live ``gtk::Picture``: ``painted`` once decoded bytes are its
          paintable, ``placeholder`` while it has none (``automation/agent.rs``).
          These two strings are the contract every ``get_attr`` app answers.
        * **macos + ios** ask the same ``get_attr``, which FaunaKit's ``PostImage``
          leaf answers off the view it is actually showing: ``painted`` once a
          decoded image — the opened sealed bytes, or ``AsyncImage``'s successful
          load of a plain blob URL — has appeared, ``placeholder`` while a spinner
          stands in for it (still loading, still sealed, or bytes that would not
          decode).
        * **windows** asks the same ``get_attr``, which maps to
          ``AutomationProperties.HelpText``. ``ImageHashBind`` (the attached
          property every ``Image`` binds its hash through) sets it off the
          ``Image``'s own current ``Source``, read back after assigning it —
          never off which branch of the load ran — on the wrapping Button that
          carries ``post-image``'s AutomationId when one is present, else on the
          ``Image`` itself.
        * **web** reads the ``<img>`` itself: ``complete && naturalWidth > 0`` holds
          only once the browser decoded it. A sealed item that did not open renders
          no ``<img>`` at all (the feed page's ``mediaUrl`` answers ``null``), and
          bytes the browser cannot decode leave ``naturalWidth`` at 0 — both read
          unpainted. The scope resolves exactly as the web bridge's
          ``_scoped_root`` does: per step, the ``index``-th ``[data-testid]`` match
          under the previous root, in document order.
        """
        d = self.driver
        if d.is_web():
            import json

            from drivers.scope import parse_scope

            steps = json.dumps([[s.element_id, s.index] for s in parse_scope(scope)])
            states = d.eval_js(
                "(() => {"
                " let root = document;"
                f" for (const [id, index] of {steps}) {{"
                "  root = root && root.querySelectorAll(`[data-testid=\"${id}\"]`)[index];"
                " }"
                " if (!root) return [];"
                " return Array.from(root.querySelectorAll('img[data-testid=\"post-image\"]'))"
                "  .map((img) => (img.complete && img.naturalWidth > 0 ? 'painted' : 'placeholder'));"
                "})()"
            )
            return [str(s) for s in (states or [])]
        n = d.count("post-image", scope=scope)
        if d.is_tui():
            return [
                "painted" if "▀" in (d.get_text("post-image", i, scope=scope) or "") else "placeholder"
                for i in range(n)
            ]
        # A `None` (the agent found no picture to read) prints as such in the
        # report, and is not `painted`.
        return [str(d.get_attr("post-image", "state", i, scope=scope)) for i in range(n)]

    def post_image_blob_hash_by_text(self, post_text: str, timeout: float = 5) -> str:
        """Get the blob hash from THE post whose body is `post_text`.

        Uses get_state to retrieve feed data which includes blob hashes,
        since element attribute reading isn't available via the bridge.

        The snapshot is refreshed by the client only after async post-body
        decoding completes (PostsDecoded on Windows, equivalent on other
        apps) — which can lag the UI's post-image visibility by a few
        hundred milliseconds. Poll for up to `timeout` seconds.

        Filters on `body` (ui.yaml's shared `feed.state_fields` contract —
        every app emits it under this key) rather than returning the first
        `has_media` row: every prior caller created exactly one image post,
        so the missing filter was a latent no-op; a caller creating TWO (an
        unsigned image, then a signed one, to compare their headers) hit it
        for real — an unresolved-vs-resolved race on the OLDER post's
        already-settled hash silently returned the wrong post's blob.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            state = self.driver.get_state("data.feed.posts")
            if state:
                for post in state:
                    if post.get("has_media") and post.get("body") == post_text:
                        # A still-resolving hash may be absent OR an explicit
                        # JSON null (tui serializes the unresolved Option as
                        # null) — both mean "keep polling", never a crash.
                        h = post.get("media_hash") or ""
                        if len(h) == 64:
                            return h
            time.sleep(0.25)
        return ""

    # --- Search ---

    def search_feed(self, query: str) -> None:
        """Type ``query`` into ``feed-search-field`` and return once the feed
        has RE-QUERIED the nest for it and that re-query has committed.

        A search is a re-query, never a client-side filter (feed.md § Where
        logic lives → *Search filter*): the nest narrows the feed, and the app
        shows what it returns. So this waits on the feed's own re-query barrier
        (``feed_reloads``, convention 14's causal anchor), which also makes the
        search tests fail on an app that filters its loaded posts locally —
        such an app starts no reload, and the barrier refuses by name.

        It settles on the NEWEST reload the typing started, not the first one to
        commit: an app may re-query per edit of the field (tui runs one search
        per field change, so clearing a previous term and typing the new one is
        two), and releasing on an intermediate commit would read a list
        narrowed by a partial or previous term.
        """
        baseline = feed_reload_baseline(self.driver)
        self.driver.clear_and_type("feed-search-field", query)
        self.driver.barrier()
        now = feed_reloads(self.driver)
        newest_started = now[0] if now is not None else baseline
        await_feed_reload_after(
            self.driver,
            None if baseline is None else max(baseline, newest_started - 1),
            budget_s=FEED_SELECT_BUDGET_S,
            what=f"searching the feed for {query!r}",
        )

    def clear_feed_search(self) -> None:
        """Tap feed-search-clear to restore normal feed."""
        if self.driver.is_visible("feed-search-clear"):
            self.driver.click("feed-search-clear")
        else:
            # Field might already be empty — clear it directly
            self.driver.clear_and_type("feed-search-field", "")

    def search_result_count(self) -> int:
        """Return the number of posts visible after a search."""
        return self.post_count()

    # --- Feed creation with specific rule types ---

    # The rule types whose form takes TWO inputs — a category and a 0–10
    # confidence — mirroring the shared `RuleInputKind::TextAndNumber`
    # (libs/fauna-client-feed/src/encoder.rs). Kept as data, not an inline
    # `in ("LabelBelow", "LabelAbove")` predicate, because that is exactly the
    # per-app ad-hoc shape the shared RuleInputKind was lifted to replace.
    TEXT_AND_NUMBER_RULE_TYPES = ("LabelBelow", "LabelAbove")

    def create_feed_with_rule(self, name: str, rule_type: str, value: str,
                              combination: str = "all",
                              required: bool = True,
                              threshold: str = "5") -> None:
        """Create a feed with a single rule.

        Args:
            name: Feed name.
            rule_type: One of the 11 FilterRule type keys (e.g. "BodyContains").
            value: Rule value (comma-separated for text types, number string for numeric).
                   For the label rules this is the CATEGORY alone ("spam") — the
                   threshold rides its own argument, not a ":"-packed string.
            combination: "all" or "any".
            required: For toggle rules (HasMedia, IsReply) — True=required, False=excluded.
            threshold: 0–10 confidence for the label rules (LabelBelow / LabelAbove),
                       typed into `feed-rule-threshold-input`. Ignored by every other
                       rule type, whose form never renders that field.
        """
        self.driver.click("feed-create-feed-button")
        self.driver.wait_for("feed-create-feed-name")
        self.driver.clear_and_type("feed-create-feed-name", name)
        self._stage_rule(rule_type, value, required=required, threshold=threshold)

        # Set combination mode if not default
        if combination != "all":
            self.driver.select("feed-combination-select", combination)

        # Submit is `.disabled(!form.canCreate || vm.creatingFeed)` on both apple
        # targets, and the just-added rule reaches `canCreate` a beat after the
        # add returns — so a bare click here drove a disabled button (measured:
        # the 2026-08-03 permissive sweep caught this exact call site).
        self.driver.wait_until_enabled("create-feed")
        self.driver.click("create-feed")
        # Wait for feed list to refresh
        time.sleep(1)

    def create_feed_with_rules(self, name: str, rules: list[tuple[str, str]],
                               combination: str) -> None:
        """Create a feed from SEVERAL rules under one combination mode — every
        rule must match (``"all"``) or any one of them (``"any"``) — the
        ``feed-combination-select`` answer `feed.md` § Feed-rule types names.

        ``rules`` is ``[(rule_type, value), …]``, each staged exactly as
        :meth:`create_feed_with_rule` stages its one. The combination is picked
        explicitly even for ``"all"``, so the form's default is never what the
        test is measuring. Returns once the new feed is listed in the selector.
        """
        self.driver.click("feed-create-feed-button")
        self.driver.wait_for("feed-create-feed-name")
        self.driver.clear_and_type("feed-create-feed-name", name)
        for rule_type, value in rules:
            self._stage_rule(rule_type, value)
        self.driver.select("feed-combination-select", combination)
        self.driver.wait_until_enabled("create-feed")
        self.driver.click("create-feed")
        self.wait_for_feed_listed(name)

    def wait_for_feed_listed(self, name: str, timeout: float = 15.0) -> None:
        """Poll until a ``feed-item`` in the selector reads ``name``."""
        deadline = time.monotonic() + timeout
        listed: list[str] = []
        while time.monotonic() < deadline:
            listed = [
                self.driver.get_text("feed-item", i) or ""
                for i in range(self.driver.count("feed-item"))
            ]
            if any(name in text for text in listed):
                return
            time.sleep(0.3)
        raise AssertionError(
            f"feed {name!r} never appeared in the selector; listed={listed!r} "
            f"error={self.driver_error_text()!r}"
        )

    def _stage_rule(self, rule_type: str, value: str, *, required: bool = True,
                    threshold: str = "5") -> None:
        """Stage one rule in the open create-feed form and add it — the per-rule
        half of :meth:`create_feed_with_rule` (see its argument notes)."""
        # Select rule type
        self.driver.select("feed-rule-type-select", rule_type)

        # Enter value based on rule type
        if rule_type in ("HasMedia", "IsReply"):
            if required:
                self.driver.click("feed-rule-required-toggle")
        else:
            self.driver.clear_and_type("feed-rule-value-input", value)
            # The label rules need the SECOND input too. Skipping it is what made
            # this helper drive a permanently-disabled add button for two whole
            # rule types: `canAddRule` requires a parseable threshold, so without
            # this line the button never enables and only `automationActivate`'s
            # bypass made the tests "pass" (found by the 2026-08-03 permissive
            # `--app ios` sweep — the ONE real offender in 1542 tests observed;
            # `feed-rule-threshold-input` approved + registered on all 7 apps
            # 2026-08-04).
            if rule_type in self.TEXT_AND_NUMBER_RULE_TYPES:
                # The threshold field renders ONLY inside the `TextAndNumber`
                # branch, so its absence here means the select above never
                # switched the branch — not that the field is broken. A bare
                # `clear_and_type` reports that as a naked 404 on
                # `feed-rule-threshold-input`, which reads as "the field is
                # missing" and sends the next session hunting the wrong element
                # (convention 6: the failure must name its own cause). Dump the
                # registry for the whole branch instead: `feed-rule-value-input`
                # present while the threshold is absent is the signature of a
                # branch that did not switch, and DUPLICATE slots for either id
                # are the signature of a stale form instance still registered
                # from a previous test (the state-leak class close_create_feed_form
                # exists for).
                if not self.driver.is_visible("feed-rule-threshold-input"):
                    tree = getattr(self.driver, "tree", lambda: "")()
                    raise AssertionError(
                        f"rule type {rule_type!r} should render "
                        f"`feed-rule-threshold-input` (the TextAndNumber branch), "
                        f"but it is absent — the `feed-rule-type-select` above did "
                        f"not switch the branch. "
                        f"value-input={self.driver.diagnose('feed-rule-value-input')} "
                        f"threshold={self.driver.diagnose('feed-rule-threshold-input')}"
                        + (f"\nregistry:\n{tree}" if tree else "")
                    )
                self.driver.clear_and_type("feed-rule-threshold-input", threshold)

        # Now a real assertion rather than a hope: with both inputs filled the
        # button MUST enable, so a failure here is a product bug, not a race.
        # (Latency-independent — a generous named ceiling, paid only on red;
        # testing.md convention 14.)
        self.driver.wait_until_enabled("feed-add-rule-button")
        self.driver.click("feed-add-rule-button")

    def close_create_feed_form(self, timeout_s: float = 10.0) -> None:
        """Close the create-feed form if it is open, leaving no state behind.

        A test that opens the form to *inspect* it (rather than submit) must call
        this, because on macOS `feed-create-feed-button` is ALSO a TOGGLE — the
        same control opens and closes the sidebar form, its label flipping to
        Cancel (`MacFeedListView.swift`) — alongside the dedicated
        `feed-create-cancel` button now built inside the form itself (
        2026-08-10; matches the other six apps). So a left-open form makes the
        NEXT test's opening click on `feed-create-feed-button` CLOSE it instead,
        and that test then fails at `wait_for("feed-create-feed-name")` with
        count=0 — which reads as "create-feed is broken" rather than "the
        previous test leaked" (measured 2026-08-04 on `--app macos`: the pair
        fails, each passes alone).

        The platform branch lives here, not in a test file (e2e convention 3).
        Idempotent: a no-op when the form is already closed.
        """
        if not self.driver.is_visible("feed-create-feed-name"):
            return
        # All 7 apps now render a real feed-create-cancel; probe rather than
        # branch on driver type (some apps ALSO keep a toggle button, harmless).
        if self.driver.is_visible("feed-create-cancel"):
            self.driver.click("feed-create-cancel")
        else:
            self.driver.click("feed-create-feed-button")
        # Assert the resulting STATE rather than trusting the click (convention
        # 14): a generous deadline poll, paid only when something is wrong.
        deadline = time.time() + timeout_s
        while time.time() < deadline and self.driver.is_visible("feed-create-feed-name"):
            time.sleep(0.2)
        assert self.driver.is_absent("feed-create-feed-name"), (
            "create-feed form still open after cancel; the next test's opening "
            f"click would close it: {self.driver.diagnose('feed-create-feed-name')}"
        )

    # ── Trained-topic training verbs (post-card ⋯ overflow;
    # topic-factors.md § Training signals) ─────────────────────────────────

    def open_post_actions(self, index: int = 0, timeout_s: float = 10.0) -> None:
        """Open post ``index``'s ⋯ overflow (``feed-post-actions-button`` →
        ``feed-post-actions-menu``). Mirrors
        ``conversations.open_message_actions``.

        Scoped to ``post-card[index]``, NOT a flat ``index=`` occurrence
        count: the flat occurrence index is REGISTRATION order (apple:
        `.onAppear` firing order, which is not guaranteed to track array
        order once a card's view identity persists across a re-rank), while
        ``post-card[index]`` is the real array-position scope every app's
        `ForEach`/list row already tags. A flat click after a re-rank could
        (and on apple demonstrably did) open a DIFFERENT post's menu than the
        one at the intended array position.
        """
        scope = f"post-card[{index}]"
        if self.driver.is_visible("feed-post-actions-menu", scope=scope):
            # Idempotent-open, not a blind toggle: the trigger button is a
            # toggle on every app (web's `actionsOpen = !actionsOpen`), so a
            # second call while already open would close it instead of
            # re-opening it — a caller that checks a verb's visibility, then
            # calls a `*_post_to_web`-style helper that opens again, must not
            # slam the menu shut out from under itself.
            return
        deadline = time.monotonic() + timeout_s
        clicked = False
        while time.monotonic() < deadline:
            try:
                if self.driver.count("feed-post-actions-button", scope=scope) > 0:
                    self.driver.click("feed-post-actions-button", scope=scope)
                    clicked = True
                    # Convention 11: the CLICK is not the outcome. A card being
                    # torn down and rebuilt under a re-rank swallows it in
                    # silence — the button was found, the click returned, and no
                    # menu opened — and the caller then times out on a verb id,
                    # naming the verb instead of the menu that never appeared.
                    # Measured on the 4th of six training gestures, where each landed train
                    # re-ranks the loaded window under the next iteration.
                    # Scoped OR unscoped, because the menu is a DESCENDANT of the
                    # card on the inline apps and a detached popup on the native
                    # ones (windows' `MenuFlyout.ShowAt`), and exactly one is ever
                    # open — the same "the one open menu is the only match" rule
                    # `train_verb_state` already reads by.
                    settle = time.monotonic() + ACTIONS_MENU_OPEN_S
                    while time.monotonic() < settle:
                        if self.driver.is_visible(
                            "feed-post-actions-menu", scope=scope
                        ) or self.driver.is_visible("feed-post-actions-menu"):
                            return
                        time.sleep(0.1)
            except Exception:  # noqa: BLE001 — settle-poll
                pass
            time.sleep(0.2)
        if clicked:
            raise TimeoutError(
                f"post-card[{index}]'s ⋯ menu never opened within {timeout_s}s: "
                "`feed-post-actions-button` was found and clicked, but "
                "`feed-post-actions-menu` never became visible. The card was "
                "most likely re-rendered under the click (a re-rank, a snapshot "
                "tick) — re-read the index from the CURRENT post order."
            )
        raise TimeoutError(f"feed-post-actions-button[{index}] never appeared")

    def train_verb_state(self, verb: str, timeout_s: float = 10.0) -> str:
        """``"on"``/``"off"`` — the training verb's checked marker (the post's
        current example marker for the in-context factor), read from the item's
        ``state`` test-attr. **The post's ⋯ menu must be OPEN**
        (``open_post_actions`` first): a closed ``gtk::Popover``'s children are
        unmapped and invisible to the element finder, so the read is unscoped —
        the one open menu is the only match. ``verb`` is ``"more"``/``"less"``."""
        el = f"feed-post-{verb}-like-this"
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            value = (self.driver.get_attr(el, "state") or "").strip().lower()
            if value:
                return value
            time.sleep(0.3)
        return ""

    def click_train_verb(self, verb: str) -> None:
        """Tap *more/less like this* in the currently-OPEN ⋯ menu —
        train-in-context (the composed feed's single trained factor). With no
        in-context factor the click opens ``feed-post-train-target-sheet``,
        which this helper deliberately does not drive — and says so, loudly,
        rather than returning as if it had trained (convention 11: a command is
        honoured or it fails on the spot).

        That second destination is why this check exists at all. It is a
        SUCCESSFUL click on the right element: no exception, no
        ``error-message``, nothing in the app log — the whole failure surfaces
        minutes later and a nest away, as a ``sample_count`` that never moves.
        A windows session spent its whole run on that shape before finding the
        gesture had opened the sheet.
        """
        el = f"feed-post-{verb}-like-this"
        self.driver.wait_for(el)
        self.driver.click(el)
        if self.driver.is_visible("feed-post-train-target-sheet"):
            raise AssertionError(
                f"tapping {el!r} opened `feed-post-train-target-sheet` instead "
                "of training: the feed's composition does not single out one "
                "`topic:*` factor, so `FeedManager::train_target_factor` "
                "returned None and NOTHING was trained (topic-factors.md "
                "§ Authoring surface & picker). Either the composed feed was "
                "not the one loaded when the ⋯ menu was opened, or it composes "
                "zero / several trained factors."
            )

    def train_post_verb(self, verb: str, post_index: int = 0) -> None:
        """Open post ``post_index``'s ⋯ menu and tap a training verb."""
        self.open_post_actions(index=post_index)
        self.click_train_verb(verb)

    # ── Own-post delete (post-card ⋯ overflow; feed.md § State & data shape
    # → Post deletion) ───────────────────────────────────────────────────

    def post_delete_visible(self, index: int = 0) -> bool:
        """Whether post ``index``'s ⋯ menu offers delete (``feed-post-delete-
        button``, gated ``is_own``). **The menu must be OPEN**
        (``open_post_actions`` first) — a closed popover's children are
        unmapped, same visibility rule as ``train_verb_state``."""
        return not self.driver.is_absent("feed-post-delete-button")

    def delete_post(self, index: int = 0, timeout_s: float = 10.0) -> None:
        """Open post ``index``'s ⋯ menu, delete it with confirm, and wait for
        the deleted post to leave the feed.

        Keyed on the post's own id (the state list is index-aligned with the
        cards — :meth:`post_index_by_id`), not on the card count falling: a
        delete also re-folds every card that embeds the deleted post (feed.md
        § Post deletion), and an app that rebuilds such a row drops the count
        for a moment while the deleted post is still there. The count is the
        fallback only when the state names no id at ``index``."""
        posts = self._feed_posts_from_state()
        target_id = posts[index].get("post_id") if 0 <= index < len(posts) else None
        initial = self.post_count()
        self.open_post_actions(index=index)
        self.driver.click("feed-post-delete-button")
        self.driver.wait_for("feed-post-delete-confirm-button")
        self.driver.click("feed-post-delete-confirm-button")
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            if target_id:
                if self.post_index_by_id(target_id) < 0:
                    return
            elif self.post_count() < initial:
                return
            time.sleep(0.3)
        raise TimeoutError(
            f"post {target_id or f'at card {index}'} did not disappear after delete"
        )

    # ── Own-post web publishing (post-card ⋯ overflow; web-content-hosting.md
    # § Published-post management) ───────────────────────────────────────

    def web_verb_visible(self, verb: str) -> bool:
        """Whether the currently-OPEN ⋯ menu offers a web-publishing verb.

        ``verb`` is one of ``publish-web`` / ``unpublish-web`` /
        ``copy-web-link`` / ``copy-paywall-link``. **The menu must be open**
        (``open_post_actions`` first) — same unmapped-popover visibility rule as
        ``train_verb_state``.
        """
        return not self.driver.is_absent(f"feed-post-{verb}-button")

    def web_verb_enabled(self, verb: str) -> bool:
        """Whether an open menu's copy verb is LIVE. It disables when the actor
        has no serving origin: publishing with no origin is legal but
        unreachable, and the UI must say so rather than hand out a dead link
        (`web-content-hosting.md` § Published-post management)."""
        return (
            self.driver.get_attr(f"feed-post-{verb}-button", "disabled") != "true"
        )

    def publish_post_to_web(self, index: int = 0) -> None:
        """Open post ``index``'s ⋯ menu and publish it as a web page (the nest
        mints the default slug)."""
        self.open_post_actions(index=index)
        self.driver.wait_for("feed-post-publish-web-button")
        self.driver.click("feed-post-publish-web-button")

    def unpublish_post_from_web(self, index: int = 0) -> None:
        """Open post ``index``'s ⋯ menu and take its published page down. One
        tap, no confirm step — unlike delete, a takedown is idempotent and
        reversible."""
        self.open_post_actions(index=index)
        self.driver.wait_for("feed-post-unpublish-web-button")
        self.driver.click("feed-post-unpublish-web-button")

    def copy_web_link(self, index: int = 0, timeout_s: float = 10.0) -> str:
        """Copy the public page URL from post ``index``'s ⋯ menu and return the
        EXACT string that reached the clipboard.

        No driver reads the OS clipboard, so the value comes back off the
        button's ``copied`` test-attr — written from the same string that was
        clipboarded, never re-derived. That attr is the only thing that lets a
        test assert the copied CONTENTS rather than the mere presence of a
        button (the devices-page lesson: unasserted copy affordances rot
        invisibly).
        """
        return self._copy_web_verb("copy-web-link", index, timeout_s)

    def copy_paywall_link(self, index: int = 0, timeout_s: float = 20.0) -> str:
        """Mint and copy a short-lived full-access link from post ``index``'s ⋯
        menu. Published **and** gated posts only. Same ``copied``-attr contract
        as :meth:`copy_web_link`, but a longer budget: unlike the public link
        this one costs a mint round trip."""
        return self._copy_web_verb("copy-paywall-link", index, timeout_s)

    def _copy_web_verb(self, verb: str, index: int, timeout_s: float) -> str:
        el = f"feed-post-{verb}-button"
        self.open_post_actions(index=index)
        self.driver.wait_for(el)
        assert self.driver.get_attr(el, "disabled") != "true", (
            f"{el} is disabled — the actor has no serving origin, so there is no "
            f"link to copy"
        )
        self.driver.click(el)
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            copied = (self.driver.get_attr(el, "copied") or "").strip()
            if copied:
                return copied
            time.sleep(0.3)
        raise TimeoutError(
            f"{el} never published a `copied` attr — nothing reached the clipboard"
        )

    def open_feed(self, name: str, timeout_s: float = 10.0) -> None:
        """Select the ``feed-item`` row whose text contains ``name``, and return
        only once that selection's own feed reload has COMMITTED.

        The barrier is convention 14's `feed_reloads` causal anchor
        (`helpers.waiting.await_feed_reload_after`), not a settle-sleep: the
        selection is asynchronous on every app (`FeedManager::select_feed`
        awaits a `reload` that resolves the composition, loads the sealed
        scorers and fetches the page), and *nothing a caller can see afterwards
        distinguishes the composed feed from the one it replaced* — with every
        nest-side key 0 (the zero-term seam) a composed feed's first page is the
        same posts in the same `created_at DESC` order the local feed already
        rendered. So a caller that polls `post_count()` is released by the
        PREVIOUS feed's posts, and every read it then makes about the composed
        feed silently answers for the old one.
        """
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            for i, item in enumerate(self.driver.get_texts("feed-item")):
                if name in (item or ""):
                    # Read the baseline in the same breath as the click: any
                    # reload already in flight (the create-feed refresh) must be
                    # counted as BEFORE, or the barrier releases on it. Through
                    # `feed_reload_baseline`, never a bare `feed_reloads` read:
                    # a native app PUSHES its state, so an unanchored triple can
                    # predate the click and make `committed_gen > baseline`
                    # satisfiable by the pre-click commit — a silent early
                    # release that proves nothing.
                    baseline = feed_reload_baseline(self.driver)
                    self.driver.click("feed-item", index=i)
                    await_feed_reload_after(
                        self.driver,
                        baseline,
                        budget_s=FEED_SELECT_BUDGET_S,
                        what=f"selecting the feed {name!r}",
                    )
                    return
            time.sleep(0.3)
        # Name the page's own error: a create the nest REFUSED (e.g. `feed limit
        # reached for your tier` on the session-scoped actor) leaves no row to
        # find, and without this the refusal reads as a render bug.
        try:
            page_error = self.driver.get_text("error-message") or ""
        except Exception as exc:  # the page may not carry one right now
            page_error = f"<unreadable: {exc}>"
        raise TimeoutError(
            f"feed-item containing {name!r} never appeared; "
            f"error-message={page_error!r}"
        )

    def return_to_feed_page(self) -> None:
        """Navigate back to the Feed page, and return only once the reload that
        the re-entry itself starts has COMMITTED.

        For a test that has already opened a feed, left for another page (to
        edit a scorer, say), and wants that SAME feed re-queried against the
        edit. ``open_feed`` is the wrong tool there: re-clicking the row that is
        still selected is not a gesture every app answers. On linux,
        ``feed-item`` activation only ever selects an UNSELECTED row
        (``views/feed/mod.rs``), so the click starts no reload and
        ``open_feed``'s barrier spends its whole budget — the
        ``test_engagement_cues`` failure in both the 2026-09-10 and 2026-09-11
        linux sweeps. Linux's nav edge does re-query the current selection
        (``main.rs``'s feed nav handler), so this anchors on that.

        It leaves for Conversations first, so the way back is a page ENTRY
        whatever page the test was on: a feed re-pulls when navigation enters
        it, not when it is re-selected while already current (:meth:`reenter`)
        — web and both apple apps start no reload for the latter, so a test
        that never left the Feed page waited out the whole budget.

        Same causal barrier as ``open_feed``: the baseline is read behind
        ``driver.barrier()`` BEFORE the navigation back, so only a reload that
        began after it can release.
        """
        self.driver.set_state({"nav": {"stack": [{"view": "conversations"}]}})
        baseline = feed_reload_baseline(self.driver)
        self.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        await_feed_reload_after(
            self.driver,
            baseline,
            budget_s=FEED_SELECT_BUDGET_S,
            what="re-entering the Feed page",
        )

    # ── Trending virtual feed (trending.md § The Trending feed) ──────────────

    def select_trending(self, timeout_s: float = 10.0) -> None:
        """Select the built-in **Trending** virtual feed (``feed-trending-item``,
        trending.md § The Trending feed) — the scored sibling of the local feed
        over ``fauna.feed.trending.posts``, driven through the shared
        ``FeedManager.select_trending_feed``. Retries the click until the row is
        present (the selector renders once the Feed page's async feed-list pull
        lands)."""
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            try:
                if self.driver.count("feed-trending-item") > 0:
                    self.driver.click("feed-trending-item")
                    return
            except Exception:  # noqa: BLE001 — settle-poll
                pass
            time.sleep(0.3)
        raise TimeoutError("feed-trending-item never appeared")

    def wait_for_first_post_text(self, text: str, timeout_s: float = 15.0) -> bool:
        """Poll until the FIRST post's rendered body contains ``text``.

        The deadline-polled twin of ``first_post_text()`` (convention 14). Reading
        ``first_post_text()`` straight after ``create_post()`` races two things that
        are not instantaneous and are not ordered with respect to the compose call:
        the new post reaching the top of the re-queried feed, and its body being
        decoded for render. Losing that race reads the PREVIOUS post — a valid body,
        just the wrong one — so the failure looks like a render bug rather than a
        test that asserted too early.

        Returns True as soon as the first post carries ``text``, else False after
        ``timeout_s`` so the caller asserts and self-diagnoses (convention 6)."""
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            try:
                if text in self.first_post_text():
                    return True
            except Exception:  # noqa: BLE001 — mid-render races are expected while polling
                pass
            time.sleep(0.3)
        return False

    def post_text_visible(self, text: str) -> bool:
        """True iff some rendered post's body contains ``text`` (element-read of
        ``post-card`` on linux/web/windows/tui; feed-state read on apple).

        The single-read primitive both waiters below poll — and the instant read
        a test uses when it already holds a **causal barrier** for the state it
        is asserting (convention 14), rather than polling for one."""
        if self._use_state_for_feed_reads():
            return any(text in p.get("body", "") for p in self._feed_posts_from_state())
        return self._find_post_index_by_text(text) >= 0

    def wait_for_post_text(self, text: str, timeout_s: float = 15.0) -> bool:
        """Poll until a post whose body contains ``text`` is rendered in the
        current feed (element-read of ``post-card`` on linux/web/windows;
        feed-state read on apple). Returns True as soon as it appears, else False
        after ``timeout_s`` — the caller asserts and self-diagnoses."""
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            if self.post_text_visible(text):
                return True
            time.sleep(0.3)
        return False

    def wait_for_post_text_absent(self, text: str, timeout_s: float = 30.0) -> bool:
        """Poll until NO rendered post's body contains ``text`` — the inverse
        twin of ``wait_for_post_text``, same reads, same self-diagnosing
        contract (True as soon as it is gone, else False at the deadline).

        This is what makes a **feed-source switch** assertable at all. The only
        discriminating UI observable between Local and Trending is that Trending
        is public-posts-only — `query_feed_scored_public` adds
        `content_meta.gated_tier IS NULL` (`trending.md` § Implementation status
        today), while the local read scopes by nothing but the spam guard — so a
        gated post the viewer sees in Local must LEAVE once the Trending read
        answers. Waiting for that departure is a positive wait on a state
        transition (convention 14: a named generous budget, deadline-polled,
        costing a green run nothing), never a settle-sleep: under the bug this
        exists to catch — staying on Local — the post never leaves and the wait
        burns its whole budget into a RED.
        """
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            if not self.post_text_visible(text):
                return True
            time.sleep(0.3)
        return False

    def create_feed_with_factor(self, name: str, factor_label: str = "engagement",
                                 weight: str = "1.0", global_scope: bool = False,
                                 combination: str = "all") -> None:
        """Create a feed with a single factor-weight entry, no filter rules
        (content-moderation-and-ranking.md § Composition).

        Args:
            name: Feed name.
            factor_label: The `feed-factor-select` option's **stable key**
                (the cross-app `select(id, value)` contract —
                `automation/agent.rs::string_model_index` — matching
                `feed-rule-type-select`'s raw `"BodyContains"`-style values)
                — `"engagement"` (always present; web matches the lowercase
                `<option value>` exactly, linux normalizes its "Engagement"
                display label onto it) or a subscribed labeler's raw factor
                string (e.g. "labeler:<hex>", appended to the picker once its
                async fetch resolves — labelers have no display name anywhere
                in the system, so their key IS their display text on both
                apps).
            weight: Decimal multiplier text (e.g. "2.0"); the client converts
                ×1000 into the wire's signed `weight_permille`.
            global_scope: Toggle `feed-factor-global-toggle` — routes the
                entry to the caller's global factor set instead of this feed.
        """
        self.driver.click("feed-create-feed-button")
        self.driver.wait_for("feed-create-feed-name")
        self.driver.clear_and_type("feed-create-feed-name", name)

        # Labeler + trained-topic options land ASYNC after the dialog opens
        # (fauna.labelers.list + the sealed-registry read) — retry the select
        # briefly so a non-"engagement" key doesn't race the fetch.
        deadline = time.monotonic() + 10
        while True:
            try:
                self.driver.select("feed-factor-select", factor_label)
                break
            except Exception:  # noqa: BLE001 — option not yet appended
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.4)
        self.driver.clear_and_type("feed-factor-weight-input", weight)
        if global_scope:
            self.driver.click("feed-factor-global-toggle")
        self.driver.click("feed-add-factor-button")

        if combination != "all":
            self.driver.select("feed-combination-select", combination)

        # Same disabled-submit window as `create_feed_with_rule` above.
        self.driver.wait_until_enabled("create-feed")
        self.driver.click("create-feed")
        time.sleep(1)
