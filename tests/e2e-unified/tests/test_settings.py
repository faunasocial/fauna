import uuid
import pytest

from common.launch_harness import reached_authenticated_app
from helpers.app_surface import skip_unbuilt
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.rpc_hold import (
    arm_rpc_hold,
    release_rpc_hold,
    rpc_hold_status,
    wait_for_held_rpc,
)
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]

#: The wire kind the Privacy page's mode read travels as — the nest's own
#: router registration (`bins/fauna-nest/src/contacts_handlers.rs`), not any
#: client-side method name. `arm_rpc_hold` rejects an unregistered spelling.
INBOX_MODE_GET_KIND = "fauna.inbox.mode.get"


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


@pytest.mark.feature("privacy-settings")
def test_inbox_mode_toggle(logged_in_app):
    """Toggle inbox mode and verify it persists after reload."""
    logged_in_app.settings.navigate()
    logged_in_app.settings.set_inbox_mode("contacts_only")
    if logged_in_app.driver.is_web():
        # Per-sub-page Settings shell: the inbox-mode radios live on the Privacy
        # sub-page, and the web agent reads `inbox_mode` from the live DOM radio —
        # so navigate away (feed) and back to Privacy, which re-mounts the page and
        # re-fetches from the nest (loadAll → getInboxMode), proving the set
        # persisted across navigation (the prior single-scroll page kept every
        # section mounted, so it navigated to Status; the shell renders one
        # sub-page at a time).
        import time
        logged_in_app.driver.navigate_to("feed")
        time.sleep(0.5)
        logged_in_app.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "privacy"}]},
        })
        time.sleep(2)
    assert logged_in_app.settings.get_inbox_mode() == "contacts_only"
    logged_in_app.settings.set_inbox_mode("open")


@pytest.mark.feature("privacy-settings")
def test_inbox_mode_pre_selects_the_accounts_real_mode_after_a_relaunch(logged_in_app):
    """The Privacy page must show the account's REAL inbox mode, not a default.

    `test_inbox_mode_toggle` above cannot see this class, and that is the whole
    reason this test exists: it reads the mode back in the *same process* that
    just set it, so a client which renders a hard-coded default and merely
    caches the user's click passes it. Only a process boundary separates "the
    page fetched and rendered the account's stored mode" from "the page is
    showing its own default and the click is still in RAM".

    Found by the audit on linux, where
    `fetch_inbox_mode`'s reply was logged and dropped while the radio group
    hard-defaulted to "open" — so Settings → Privacy told every account its
    inbox was open to everyone. Enforcement is nest-side, so the setting itself
    was honoured; what broke was the user's ability to see and trust it, which
    on a privacy control is not cosmetic.

    Non-default mode on purpose: "open" is every app's default paint, so a
    broken client would pass an assertion written against it.
    """
    driver = logged_in_app.driver
    settings = logged_in_app.settings

    settings.navigate()
    settings.set_inbox_mode("contacts_only")
    assert settings.get_inbox_mode() == "contacts_only", (
        "precondition: the mode must actually be set before a relaunch can "
        "prove the page re-reads it"
    )

    # Two separate preconditions, and conflating them costs a confusing failure
    # several steps from the cause (it did, on web, 2026-08-05):
    #   1. the client STORE must survive the relaunch, else the app returns to a
    #      fresh one and this fails for the wrong reason
    #      (`e2e-relaunch-wipes-client-store`);
    #   2. the injected identity must survive INTO that store, else the app
    #      returns signed out and never reaches the page under test.
    if not driver.preserve_state_across_relaunch():
        skip_unbuilt(
            driver,
            surface="a client store that can be pinned across a relaunch",
            detail=(
                "the driver hands the relaunched process a fresh store, so the "
                "app returns signed out and cannot re-read the stored mode"
            ),
            tracked="drivers/http_bridge.py::preserve_state_across_relaunch",
        )
    if not driver.relaunch_preserves_injected_identity():
        skip_unbuilt(
            driver,
            surface="a login that survives a relaunch",
            detail=(
                "logged_in_app injects the session with set_state, and on this "
                "app the injection does not reach a store the relaunch keeps, so "
                "the app returns signed out and the stored inbox mode can never "
                "be re-read. The product behaviour here is UNVERIFIED, not "
                "known-good"
            ),
            tracked="drivers/http_bridge.py::relaunch_preserves_injected_identity",
        )
    assert driver.recover(), "the app did not come back up after the relaunch"
    reached_authenticated_app(driver, timeout=90)

    settings.open_privacy()

    # Deadline poll, not a settle-sleep (testing.md convention 14): the re-read
    # is one WS-RPC round trip whose latency is irrelevant to correctness, so a
    # green run returns instantly and only a genuine failure spends the budget.
    def _diagnose():
        seen = settings.get_inbox_mode()
        return (
            "after a relaunch the Privacy page must show the account's stored "
            f"inbox mode, but it reports {seen!r}. "
            + (
                "An empty value means the page never learned the mode at all; "
                if not seen
                else f"{seen!r} is a value the user never chose — "
            )
            + "the nest still holds 'contacts_only', so this is a privacy "
            f"control displaying someone else's answer. "
            f"error={logged_in_app.error_text()!r}"
        )

    wait_until(
        lambda: settings.get_inbox_mode() == "contacts_only",
        RPC_ROUNDTRIP_S,
        diagnose=_diagnose,
    )


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("privacy-settings")
def test_inbox_mode_is_unknown_while_its_fetch_is_still_pending(logged_in_app, nest_instance):
    """While `inbox_mode_get` is unanswered, the page reports NO mode at all.

    The test above proves the page eventually shows the account's stored mode.
    It cannot see the window before that — and the window is where the rule
    lives: `settings.md` § Privacy sub-page requires that "Until
    ``inbox_mode_get`` has answered, the mode is *unknown*", because a privacy
    control that paints a plausible guess and corrects itself a round trip later
    has told the user something false about who can reach them. Every
    post-fetch assertion in the suite passes on such a client, since by the time
    one looks the guess is gone.

    Standing inside that window needs the reply held, not raced. A zero-wait read
    right after navigating is the wall-clock race `e2e-conventions.md` point 14
    calls DEFUNCT (a fast local nest answers before the next line runs, so the
    assertion passes only on the runs where it passes at all), and freezing the
    nest is forbidden outright. `helpers/rpc_hold` holds the one kind pending at
    the nest's dispatch chokepoint until this test releases it, and reports when
    the request has actually arrived — so the window becomes a state to stand in
    with no deadline of its own.

    Deliberately asserts the *mode*, not a rendering: the apps legitimately
    differ on whether the Privacy page is on screen during the window (linux and
    web build it at once and repaint; tui awaits the fetch on the nav edge), and
    both shapes satisfy the rule. What no app may do is name a mode.

    MARKED for the six apps that expose a pending page, and tui is not among
    them (`SettingsActions.privacy_nav_awaits_its_mode_read`): tui awaits the
    read on the nav edge, so holding the reply makes the NAV block instead of
    exposing a page to read a mode off — this test cannot drive tui, which is
    a mark and never a `declared_absence` (feature-catalog.md § Cell
    semantics, the marked-witness rule, 2026-09-26). tui satisfies the rule
    structurally, and its own witness of it is
    `test_privacy_nav_names_no_mode_while_the_mode_read_is_held` below.
    """
    driver = logged_in_app.driver
    settings = logged_in_app.settings
    port = nest_instance["port"]

    settings.navigate()
    settings.set_inbox_mode("contacts_only")
    assert settings.get_inbox_mode() == "contacts_only", (
        "precondition: the mode must actually be set, or the release half of "
        "this test cannot tell a correct answer from a lucky default"
    )

    # A relaunch, and for the same two reasons the test above needs one: only a
    # process boundary clears the mode this test just cached, and a client that
    # kept the mode in RAM would enter the held window already knowing it.
    if not driver.preserve_state_across_relaunch():
        skip_unbuilt(
            driver,
            surface="a client store that can be pinned across a relaunch",
            detail=(
                "the driver hands the relaunched process a fresh store, so the "
                "app returns signed out and never issues the read this test holds"
            ),
            tracked="drivers/http_bridge.py::preserve_state_across_relaunch",
        )
    if not driver.relaunch_preserves_injected_identity():
        skip_unbuilt(
            driver,
            surface="a login that survives a relaunch",
            detail=(
                "logged_in_app injects the session with set_state, and on this "
                "app the injection does not reach a store the relaunch keeps, so "
                "the app returns signed out and never reaches the page under test"
            ),
            tracked="drivers/http_bridge.py::relaunch_preserves_injected_identity",
        )
    # Armed BEFORE the relaunch, and this is the load-bearing ordering. The only
    # genuinely-unknown window is a COLD one — an app that already holds the mode
    # is not guessing, it is remembering, and re-entering the page does not
    # unlearn it. Arming after the relaunch loses the race whenever the app reads
    # the mode during its own start-up: measured 2026-08-27 on linux, where this
    # test passed run-alone and failed behind the two tests above, whose
    # navigation left the app restoring the Privacy page on relaunch — it came
    # back already holding "contacts_only", with the nest never asked again.
    # Arming first makes every read of this kind parked from before the process
    # exists, so no ordering can leak an answer in.
    arm_rpc_hold(port, INBOX_MODE_GET_KIND)
    try:
        assert driver.recover(), "the app did not come back up after the relaunch"
        reached_authenticated_app(driver, timeout=90)

        # `request_privacy`, not `open_privacy`: waiting for the page to render
        # would hang on every app that (correctly) declines to render it until
        # the read answers.
        settings.request_privacy()

        # The step that makes this deterministic rather than merely slower —
        # a deadline poll on latency-independent state (the nest is holding N
        # requests of this kind *right now*), never a settle-sleep. Without it
        # the assertion below could run before the app had issued the read at
        # all, and would be measuring startup speed instead of the rule.
        def _diagnose_arrival():
            return (
                f"app-side: nav={driver.get_state('nav')!r} "
                f"settings={driver.get_state('settings')!r} "
                f"session={driver.get_state('session')!r}"
            )

        wait_for_held_rpc(port, INBOX_MODE_GET_KIND, diagnose=_diagnose_arrival)

        seen = settings.get_inbox_mode()
        assert seen == "", (
            "while inbox_mode_get is still pending the Privacy page must name "
            f"no mode, but it reports {seen!r}. The nest is holding the reply — "
            "so this is not a value the app read from anywhere, it is a value "
            "the app made up. On a privacy control that is a claim about who can "
            "reach the user, made before a single request to check. "
            f"error={logged_in_app.error_text()!r}"
        )
        assert rpc_hold_status(port, INBOX_MODE_GET_KIND)["holding"] >= 1, (
            "the reply was released before the assertion above ran, so it "
            "proved nothing about the pending window"
        )
    finally:
        release_rpc_hold(port, INBOX_MODE_GET_KIND)

    # And the other half: released, the page converges on the stored mode. This
    # is what separates "never guesses" from "never shows anything".
    settings.open_privacy()
    wait_until(
        lambda: settings.get_inbox_mode() == "contacts_only",
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            "once the held reply is released the page must show the stored "
            f"mode, but it reports {settings.get_inbox_mode()!r}"
        ),
    )


@pytest.mark.tui
@pytest.mark.feature("privacy-settings")
def test_privacy_nav_names_no_mode_while_the_mode_read_is_held(logged_in_app, nest_instance):
    """tui's witness of the same rule: while `inbox_mode_get` is held, the
    Privacy nav names no mode — and once released, the page shows the real one.

    The six-app test above cannot drive tui (its docstring says why): tui awaits
    `Op::FetchPrivacy` on the nav edge (`apps/tui.md`, one awaited `Op` per nav
    edge), so there is no pending *page* to read a mode off — the nav itself is
    what waits. tui satisfies `settings.md` § Privacy sub-page item 7 ("Marking
    a mode nobody fetched is forbidden") structurally, and this test is the
    witness that the structure holds: a tui that painted the page first and
    patched the mode in later would name a guess here.

    The observable is the one channel tui's automation server keeps answering
    while the nav's op is pending — `/app/state`, a snapshot read under a mutex,
    off the app loop. Element reads and the command's ack both run on the loop
    the op is blocking, so the nav is driven from a thread and the main thread
    reads state only.

    Two shapes are correct, and the test accepts both. The client's own RPC
    deadline bounds the wait (measured 2026-08-27: the nav acked ~5s later
    carrying a "took too long" error, the request abandoned), after which the
    nav completes and paints all four modes unmarked. So the assertion is the
    rule — no mode named while the nest still holds the read — never "the nav
    does not complete", which would be a claim about the deadline, not the rule.
    For the same reason the release alone paints nothing: an abandoned call's
    late reply has nowhere to go, so the page is re-opened to converge.
    """
    import threading

    driver = logged_in_app.driver
    settings = logged_in_app.settings
    port = nest_instance["port"]

    settings.navigate()
    settings.set_inbox_mode("contacts_only")
    assert settings.get_inbox_mode() == "contacts_only", (
        "precondition: the mode must actually be set, or the release half of "
        "this test cannot tell a correct answer from a lucky default"
    )

    # A relaunch, for the reasons the six-app test gives: only a process
    # boundary clears the mode this test just cached. tui pins its store across
    # a relaunch and keeps the injected login, so these guards are the six-app
    # test's own, kept so a driver change reads as a skip rather than a red.
    if not driver.preserve_state_across_relaunch():
        skip_unbuilt(
            driver,
            surface="a client store that can be pinned across a relaunch",
            detail="the relaunched process would get a fresh store and return signed out",
            tracked="drivers/tui.py::preserve_state_across_relaunch",
        )
    if not driver.relaunch_preserves_injected_identity():
        skip_unbuilt(
            driver,
            surface="a login that survives a relaunch",
            detail="the relaunched app would return signed out and never reach the page",
            tracked="drivers/http_bridge.py::relaunch_preserves_injected_identity",
        )
    # Armed BEFORE the relaunch, as the six-app test does and for its reason:
    # only a cold app is genuinely unknowing, so every read of this kind must be
    # parked from before the process exists.
    arm_rpc_hold(port, INBOX_MODE_GET_KIND)
    nav_error: list[BaseException] = []
    try:
        assert driver.recover(), "the app did not come back up after the relaunch"
        reached_authenticated_app(driver, timeout=90)

        # The nav's ack waits on the op the hold is blocking, so it runs off the
        # main thread; the main thread observes through `/app/state` only.
        def _nav():
            try:
                settings.request_privacy()
            except BaseException as e:  # surfaced after the join below
                nav_error.append(e)

        nav = threading.Thread(target=_nav, name="privacy-nav", daemon=True)
        nav.start()

        # Convention 14's anchor: the held request's ARRIVAL at the nest — the
        # app has issued the read — never a settle-sleep.
        def _diagnose_arrival():
            return (
                f"app-side: nav={driver.get_state('nav')!r} "
                f"settings={driver.get_state('settings')!r} "
                f"session={driver.get_state('session')!r}"
            )

        wait_for_held_rpc(port, INBOX_MODE_GET_KIND, diagnose=_diagnose_arrival)

        # `get_state`, not `get_inbox_mode`: the action's element-scan fallback
        # would queue on the very loop the held op is blocking.
        seen = driver.get_state("settings")
        assert isinstance(seen, dict), (
            f"tui's /app/state must answer while the nav's op is pending; got {seen!r}"
        )
        assert (seen.get("inbox_mode") or "") == "", (
            "while inbox_mode_get is held at the nest, tui must name no mode — "
            f"but its state reports {seen.get('inbox_mode')!r}. The nest has "
            "not answered, so that value was made up, and on a privacy control "
            "it is a claim about who can reach the user. "
            f"nav={driver.get_state('nav')!r}"
        )
        assert rpc_hold_status(port, INBOX_MODE_GET_KIND)["holding"] >= 1, (
            "the reply was released before the assertion above ran, so it "
            "proved nothing about the pending window"
        )
    finally:
        release_rpc_hold(port, INBOX_MODE_GET_KIND)

    # The nav acks either on the released reply or on the client's own deadline;
    # both are well inside the driver's ack budget, whose TimeoutError the
    # thread would have captured.
    nav.join()
    assert not nav_error, f"the Privacy nav failed to ack: {nav_error[0]!r}"

    # Released, the page converges on the stored mode — what separates "never
    # guesses" from "never shows anything".
    settings.open_privacy()
    wait_until(
        lambda: settings.get_inbox_mode() == "contacts_only",
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            "once the held reply is released the page must show the stored "
            f"mode, but it reports {settings.get_inbox_mode()!r}"
        ),
    )


#: The Privacy page's mode write — the kind a write-back on page open would send.
INBOX_MODE_SET_KIND = "fauna.inbox.mode.set"


@pytest.mark.feature("privacy-settings")
def test_opening_the_privacy_page_never_writes_the_inbox_mode_back(
    logged_in_app, nest_instance
):
    """Showing the stored inbox mode is not a user choice, so opening the
    Privacy page sends no `inbox_mode_set` (`settings.md` § Layout & flow
    item 7: "Displaying the stored mode must not write it back").

    A client whose radio group fires its change handler when the page paints
    the fetched selection writes the mode back on every visit — harmless while
    the value is right, and a silent overwrite the moment it is stale. Nothing
    the user can see tells the two apart, so the nest is the witness: every
    `fauna.inbox.mode.set` is parked at the dispatch chokepoint
    (`helpers/rpc_hold`) and counted.

    The negative needs a fence, not a quiet period (point 14): after the page
    has painted the stored mode, the user makes a real choice, and that
    request must be the FIRST — and only — write the nest ever sees. A
    write-back from the page open was sent before the user could click, so it
    would be parked ahead of it and the count would read two."""
    driver = logged_in_app.driver
    settings = logged_in_app.settings
    port = nest_instance["port"]

    settings.navigate()
    settings.set_inbox_mode("contacts_only")
    assert settings.get_inbox_mode() == "contacts_only", (
        "precondition: a non-default stored mode, so a painted default cannot "
        "pass for the stored one"
    )
    settings._navigate_subpage("status")

    arm_rpc_hold(port, INBOX_MODE_SET_KIND)
    try:
        settings.open_privacy()
        wait_until(
            lambda: settings.get_inbox_mode() == "contacts_only",
            RPC_ROUNDTRIP_S,
            diagnose=lambda: f"the page shows {settings.get_inbox_mode()!r}",
        )
        # The user's own choice, sent WITHOUT waiting for it to land: the write
        # is held, so a gesture that awaits it would block until the client
        # gave up — and a timed-out request drops out of the nest's hold count
        # before the arrival wait below could see it (measured on tui,
        # 2026-09-22: `holding` read 0 with the page showing "took too long").
        # tui's keyboard activation runs the write off the render loop as a
        # user's keypress does (`FeedActions.start_submit`'s precedent). The
        # other apps click; one whose click likewise awaits the write fails at
        # the arrival wait, naming that app's gap rather than passing without it.
        if driver.is_tui():
            driver.press_key("inbox-mode-allow_knock", "Enter")
        else:
            driver.click("inbox-mode-allow_knock")
        wait_for_held_rpc(
            port,
            INBOX_MODE_SET_KIND,
            diagnose=lambda: (
                f"the page shows {settings.get_inbox_mode()!r}; error-message "
                f"{logged_in_app.error_text()!r}"
            ),
        )
        status = rpc_hold_status(port, INBOX_MODE_SET_KIND)
        assert status.get("holding") == 1, (
            "opening the Privacy page wrote the inbox mode back: the nest "
            f"received {status.get('holding')} mode writes where only the "
            f"user's own choice should be (status {status!r})"
        )
    finally:
        release_rpc_hold(port, INBOX_MODE_SET_KIND)

    # The user's choice itself applies once released — the fence was a real
    # write, not a lost one — and the shared user goes back to its default.
    wait_until(
        lambda: settings.get_inbox_mode() == "allow_knock",
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the page shows {settings.get_inbox_mode()!r}",
    )
    settings.set_inbox_mode("open")


@pytest.mark.feature("privacy-settings")
def test_spam_preferences(logged_in_app):
    """Adjust spam preferences, save, verify section visible."""
    logged_in_app.settings.navigate()
    logged_in_app.settings.save_spam_preferences(
        spam_threshold="0.7",
        phishing_threshold="0.6",
    )
    # After save, verify the save button is still on screen (section rendered).
    # On iOS, the spam-preferences heading may scroll offscreen during interaction.
    assert (logged_in_app.driver.is_visible("spam-preferences")
            or logged_in_app.driver.is_visible("save-spam-prefs")), (
        "spam-preferences section should stay rendered after save: "
        f"section={logged_in_app.driver.diagnose('spam-preferences')} "
        f"save={logged_in_app.driver.diagnose('save-spam-prefs')}"
    )


@pytest.mark.web  # a witness that can drive only web is MARKED for it, never gated by
# declared_absence on the others: an absence names a behaviour a platform lacks, a mark
# names the apps a test can drive (feature-catalog.md § Cell semantics)
@pytest.mark.feature("privacy-settings")
def test_spam_preferences_persist_web(logged_in_app):
    """Spam preferences survive a page re-mount — exercises BOTH
    `fauna.spam.set_preferences` (save) and `fauna.spam.get_preferences` (re-hydrate)
    end-to-end over the WS-RPC `SpamClient` seam.

    Strategy: the phishing threshold defaults to `0.3` server-side, so changing it to
    `0.7`, saving, re-mounting the page, and then seeing `0.7` come back can only mean
    the set+get round-trip persisted it (a non-default value can't be the component
    default). The page exposes no signal for "onMount's `loadAll()` has landed", and a
    load landing after the slider is set would put the default back — so the whole
    set → save → re-mount → read-back sequence repeats until the saved value comes
    back, which it does on the first pass a load does not race.

    Web-only: reads the live `.value` property via `eval_js` (no JS context on the other
    apps — a structural impossibility). linux's already-migrated path keeps its coverage
    via `test_spam_preferences` above.
    """
    import time
    driver = logged_in_app.driver

    PHISHING = 'document.querySelector(\'[data-testid="phishing-threshold"]\').value'
    SAVED = "(()=>{const e=document.querySelector('.success');return e?e.textContent:'';})()"

    def poll(expr: str, want, timeout: float = 15.0):
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            last = driver.eval_js(expr)
            if want(last):
                return last
            time.sleep(0.5)
        return last

    # Spam preferences live on the Privacy sub-page of the Settings sidebar-swap
    # shell — navigate there (the shell renders one sub-page at a time).
    _SPAM = {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "privacy"}]}}
    driver.set_state(_SPAM)
    driver.wait_for("spam-preferences")

    def is_saved_value(v) -> bool:
        return v is not None and float(v) == 0.7

    got = saved = None
    for _attempt in range(4):
        driver.clear_and_type("phishing-threshold", "0.7")  # change off the 0.3 default
        driver.click("save-spam-prefs")  # → fauna.spam.set_preferences
        # The page's "Saved!" confirmation appears only AFTER the set_preferences
        # WS round-trip resolves (nest persisted).
        saved = poll(SAVED, lambda v: bool(v) and "aved" in str(v))
        assert saved and "aved" in str(saved), f"save did not confirm (got {saved!r})"

        # Re-mount the settings page (navigate away + back) so onMount → loadAll →
        # fauna.spam.get_preferences re-fetches from the nest over the live WS
        # connection — a cleaner re-hydration than a full reload (no reconnect / no
        # identity re-init), so the round-trip is what's under test, not the harness.
        driver.navigate_to("feed")
        driver.set_state(_SPAM)
        driver.wait_for("spam-preferences")

        got = poll(PHISHING, is_saved_value)  # until get re-hydrates the saved value
        if is_saved_value(got):
            break
    assert is_saved_value(got), (
        f"the phishing threshold must persist across re-mount (set+get round-trip); got {got!r}"
    )


@pytest.mark.feature("mail-filter-rules")
def test_email_filter_crud(logged_in_app):
    """Create and delete email filters."""
    logged_in_app.settings.navigate()
    initial = logged_in_app.settings.filter_count()

    logged_in_app.settings.create_email_filter(
        name="Block spam", rule_type="SenderIs",
        rule_value="spammer@evil.com", action="Discard"
    )
    assert logged_in_app.settings.filter_count() == initial + 1
    assert "Block spam" in logged_in_app.settings.filter_names()

    logged_in_app.settings.create_email_filter(
        name="Allow trusted", rule_type="SenderIs",
        rule_value="friend@good.com", action="Allow"
    )
    assert logged_in_app.settings.filter_count() == initial + 2

    logged_in_app.settings.delete_filter(0)
    assert logged_in_app.settings.filter_count() == initial + 1

    logged_in_app.settings.delete_filter(0)
    assert logged_in_app.settings.filter_count() == initial


@pytest.mark.feature("mail-filter-rules")
def test_email_filter_edit(logged_in_app):
    """Edit an existing filter's name/rule-value/action in place (no
    delete+recreate) and confirm the change round-trips through the server —
    not an optimistic client-side row edit (the exact bug filter-delete had
    before this feature: it removed the row locally and never called the
    server at all)."""
    logged_in_app.settings.navigate()
    name = _unique("Edit me")
    logged_in_app.settings.create_email_filter(
        name=name, rule_type="SenderIs",
        rule_value="before@example.com", action="Discard"
    )
    names = logged_in_app.settings.filter_names()
    assert name in names, f"create didn't land; have {names}"
    index = names.index(name)

    assert logged_in_app.settings.filter_edit_visible(index), (
        "a filter the create dialog itself produced must offer an edit affordance"
    )

    new_name = _unique("Edited")
    logged_in_app.settings.edit_filter(
        index, name=new_name, rule_type="SenderDomain",
        rule_value="after.example.com", action="Allow"
    )

    names = logged_in_app.settings.filter_names()
    assert new_name in names, f"edit did not rename the filter; have {names}"
    assert name not in names, "the pre-edit name must be gone, not duplicated"

    new_index = names.index(new_name)
    assert logged_in_app.settings.filter_action(new_index) == "Allow", (
        "edited action did not round-trip"
    )

    # Force a real re-mount (not just the in-memory row the mutation's own
    # response already rendered) to prove the edit reached the server via
    # fauna.email.filters.update, matching test_inbox_mode_toggle's method.
    if logged_in_app.driver.is_web():
        import time
        logged_in_app.driver.navigate_to("feed")
        time.sleep(0.5)
        logged_in_app.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "privacy"}]},
        })
        time.sleep(2)

    names_after_remount = logged_in_app.settings.filter_names()
    assert new_name in names_after_remount, (
        f"edit did not persist server-side; have {names_after_remount}"
    )

    # Cleanup.
    logged_in_app.settings.delete_filter(logged_in_app.settings.filter_names().index(new_name))


def _account_filters(nest_instance, user) -> list[dict]:
    """The account's stored filters, read straight off the nest over the
    account's own `fauna.email.filters.list` (convention 5): what the app
    wrote, not what it painted."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    ) as account:
        return account.call("fauna.email.filters.list", {})["filters"]


def _stored_filter(nest_instance, user, name: str) -> dict:
    matches = [f for f in _account_filters(nest_instance, user) if f["name"] == name]
    assert len(matches) == 1, f"expected one stored filter named {name!r}; have {matches}"
    return matches[0]


def _wait_filter_listed(settings, name: str) -> int:
    """The row index of the filter named `name`, once the list shows it."""
    names = wait_until(
        lambda: (lambda n: n if name in n else None)(settings.filter_names()),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"have {settings.filter_names()}",
    )
    return names.index(name)


@pytest.mark.feature("mail-filter-rules")
def test_email_filter_forward_keeps_its_copy_mode_through_an_edit(
    logged_in_app, nest_instance, test_user
):
    """A Forward rule created with "keep a local copy" unchecked is stored as a
    redirect, listed with the Forward badge, reopens with the box still
    unchecked, and saving the untouched form leaves it a redirect —
    `mail-filter-rules` outcomes 4 (forward), 7 (never opens in a lossy form)
    and 8 (the copy mode survives an edit)."""
    settings = logged_in_app.settings
    settings.navigate()
    name = _unique("Forward away")
    destination = f"{_unique('dest')}@example.net"
    settings.create_email_filter(
        name=name, rule_type="SenderIs", rule_value="boss@example.com",
        action="Forward", forward_address=destination, keep_local_copy=False,
    )
    index = _wait_filter_listed(settings, name)
    assert settings.filter_action(index) == S.status.email_filters.action_forward
    expected = {"Forward": {"address": destination, "redirect": True}}
    stored = _stored_filter(nest_instance, test_user, name)
    assert stored["action"] == expected, (
        f"the unchecked box must store a redirect; stored {stored['action']!r}"
    )

    assert settings.filter_edit_visible(index), (
        "a Forward rule the form itself produced must offer an edit affordance"
    )
    settings.open_filter_edit(index)
    assert settings.filter_forward_address() == destination
    assert not settings.filter_keep_local_copy(), (
        "the edit form must reopen a redirect rule with 'keep a local copy' unchecked"
    )
    settings.save_open_filter()

    after = _stored_filter(nest_instance, test_user, name)
    assert after["action"] == expected, (
        f"saving the untouched edit form changed the action to {after['action']!r}"
    )

    settings.delete_filter(_wait_filter_listed(settings, name))


@pytest.mark.feature("mail-filter-rules")
def test_email_filter_the_form_cannot_show_is_listed_but_never_opens(
    logged_in_app, nest_instance, test_user
):
    """A rule the form cannot fully show — an auto-reply, which only the API
    writes today — is listed with its badge but offers no edit affordance, so
    it never opens in a form that would lose part of it (`mail-filter-rules`
    outcome 7)."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    name = _unique("Away notice")
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=test_user["actor_id_bytes"],
        signing_key=bytes(test_user["signing_key"]),
    ) as account:
        account.call("fauna.email.filters.create", {
            "name": name,
            "rules": [{"SenderDomain": {"domain": "example.org"}}],
            "combination": "all",
            "action": {"AutoReply": {
                "subject": "Away", "body": "Back on Monday", "interval_hours": 24,
            }},
            "priority": 0,
        })

    settings = logged_in_app.settings
    settings.navigate()
    index = _wait_filter_listed(settings, name)
    assert settings.filter_action(index) == S.status.email_filters.action_auto_reply
    assert not settings.filter_edit_visible(index), (
        "an auto-reply rule must not open in a form that cannot hold its subject/body"
    )

    settings.delete_filter(index)


@pytest.mark.feature("account")
def test_handle_change_validation(logged_in_app):
    """A malformed handle surfaces a validation error and is not applied.

    The format rules live once in shared Rust
    (`fauna_protocol::handle::validate_handle`): the nest enforces them on
    `fauna.profile.handle.change`, and clients call the *same* validator
    pre-submit (linux natively; web/native via the wasm/UniFFI export). A client
    that has not yet wired the client-side check still surfaces the nest's
    rejection — either way the user must see the error rather than a silent no-op.

    `ab` is too short (< 3 chars), so it can never be a valid handle and the
    assertion never mutates the logged-in actor's real handle.

    Cross-app rollout: linux is the reference (validates client-side + shows
    the error in `error-message`). Where a client doesn't yet expose the
    change-handle form *or* doesn't yet route the change-handle error into the
    `error-message` element, the test skips with an *adoption-pending* reason
    (tracked in `NEXT-<client>` + settings.md § Implementation status today) —
    e.g. web gates the form behind `{#if identity.handle}`. As each app
    adopts, it runs and asserts automatically. linux must never skip here.
    """
    import time

    d = logged_in_app.driver
    logged_in_app.settings._navigate_subpage("account")
    try:
        d.wait_for("new-handle", timeout=5)
    except Exception:
        pytest.skip(
            "change-handle form (new-handle) not reachable for this client — "
            "adoption pending (e.g. web gates it on identity.handle); see NEXT-<client>"
        )
    d.clear_and_type("new-handle", "ab")
    d.click("change-handle")
    time.sleep(2)
    # Read the `error-message` element directly. The cross-app `error_text()`
    # helper consults the state protocol first, which can shadow a *page-level*
    # error (not the global banner) on clients that report a `messages` object.
    if not d.is_visible("error-message"):
        pytest.skip(
            "client does not yet route the change-handle validation error into "
            "the error-message element — adoption pending; see NEXT-<client>"
        )
    err = d.get_text("error-message")
    assert err, f"error-message visible but empty (got {err!r})"


@pytest.mark.feature("account")
def test_handle_someone_else_holds_is_refused_and_not_applied(
    logged_in_app, nest_instance, test_user
):
    """A handle another account already holds is refused with the nest's
    reason, and nothing is applied or scheduled (`settings.md` § User actions).

    `test_handle_change_validation` above types a too-short handle precisely so
    it never reaches the nest; this is the other half — a well-formed handle
    that only the nest can refuse, because only the nest knows who holds it
    (`fauna.profile.handle_taken`, checked before any pending action is
    created). A second account is registered holding the wanted handle; the
    user asks for it through the Account page's own form.

    The mutation is UI; the verification is the sanctioned black-box read
    (convention 8's carve-out): the handle still resolves to its holder and
    the user's own handle to the user. The refusal is a synchronous reply, so
    waiting for the error line is waiting on state, not on time (point 14)."""
    from common import create_actor_and_register
    from tests.test_pending_actions import _resolve_handle

    holder = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    wanted = holder["handle"]

    d = logged_in_app.driver
    logged_in_app.settings._navigate_subpage("account")
    try:
        d.wait_for("new-handle", timeout=5)
    except Exception:
        skip_unbuilt(
            d,
            surface="new-handle",
            detail="the change-handle form is not reachable on this app",
            tracked="",
        )
    pending_before = d.count("pending-action-item")

    d.clear_and_type("new-handle", wanted)
    d.click("change-handle")
    # Scrolled: the form's button is what the user just pressed, so the page sits
    # scrolled down to it — and on windows a bare `is_visible` reads the error bar
    # (at the top of the page) as absent while it is painted one scroll above.
    err = wait_until(
        lambda: d.is_visible_scrolled("error-message") and d.get_text("error-message"),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"no refusal painted; pending rows {d.count('pending-action-item')}, "
            f"state errors {(d.get_state() or {}).get('messages')!r}"
        ),
    )
    assert err == S.error.profile.handle_taken, (
        "the refusal must carry the nest's reason — the handle belongs to "
        f"someone else — not a generic failure; got {err!r}"
    )

    assert _resolve_handle(nest_instance["url"], wanted) == holder["actor_id_hex"], (
        "the wanted handle must still belong to the account that holds it"
    )
    assert _resolve_handle(nest_instance["url"], test_user["handle"]) == test_user[
        "actor_id_hex"
    ], "the user's own handle must be unchanged by a refused change"
    descriptions = [
        d.get_text("pending-action-description", index=i)
        for i in range(d.count("pending-action-item"))
    ]
    assert d.count("pending-action-item") == pending_before and not any(
        wanted in text for text in descriptions
    ), f"a refused handle change must schedule nothing; rows now read {descriptions!r}"


@pytest.mark.feature("account")
def test_actor_id_visible(logged_in_app):
    """Verify actor ID is displayed on the settings page.

    Navigates straight to the Status sub-page, not just `.navigate()`: iOS's
    idiomatic settings nav (settings.md § Navigation model — Mobile)
    intentionally lands a plain `.navigate()` on the root page *list*, not
    any sub-page (`SettingsPage.swift`'s `init(navId:)` doc comment), so the
    identity data is unreachable without this. **Status, not Account**:
    `account-actor-id` is a documented platform divergence, not a uniform
    Account-page id — macOS's `AccountSettingsView` gates its Identity
    section `#if os(iOS)` (identity lives on `MacStatusView` instead;
    `AccountSettingsView.swift`'s doc comment), and iOS's own Status sub-page
    (`SettingsView.swift`) carries the same id too, so Status is the one
    sub-page every app renders it on. `test_handle_change_validation`
    still uses `_navigate_subpage("account")` correctly — it drives an
    Account-only action (handle change), not identity display.
    """
    logged_in_app.settings._navigate_subpage("status")
    actor_id = logged_in_app.settings.actor_id()
    assert actor_id, "Actor ID should be displayed on settings page"


@pytest.mark.feature("account")
def test_copy_buttons_visible(logged_in_app):
    """Verify copy buttons are present on the settings page.

    See `test_actor_id_visible` for why this navigates to the Status
    sub-page rather than Account or relying on `.navigate()`'s default.
    """
    logged_in_app.settings._navigate_subpage("status")
    # At least one copy button should be visible
    has_copy = (logged_in_app.driver.is_visible("account-actor-id-copy-btn")
                or logged_in_app.driver.is_visible("status-actor-id-copy-btn")
                or logged_in_app.driver.is_visible("status-node-url-copy-btn"))
    assert has_copy, (
        "at least one copy button should be visible: "
        f"account={logged_in_app.driver.diagnose('account-actor-id-copy-btn')} "
        f"status-actor={logged_in_app.driver.diagnose('status-actor-id-copy-btn')} "
        f"status-node={logged_in_app.driver.diagnose('status-node-url-copy-btn')}"
    )


def _copy_and_read_back(app, button_id: str) -> str:
    """Click one copy button and return what it reports having copied (its
    ``copied`` attr — written from the value that reached the clipboard, the
    copy-button contents contract ``actions/profile.py`` documents)."""
    app.driver.click(button_id)
    return wait_until(
        lambda: app.driver.get_attr(button_id, "copied") or None,
        15.0,
        diagnose=lambda: (
            f"{button_id} never reported what it copied; "
            f"{app.driver.diagnose(button_id)} error={app.error_text()!r}"
        ),
    )


@pytest.mark.feature("status-quotas-and-limits")
def test_status_copy_buttons_copy_your_identity_and_your_nests_address(
    logged_in_app, nest_instance
):
    """`status.md` § Layout & flow: the Status page lets you copy your identity
    and your nest's address — and each button copies the RIGHT value.

    `test_copy_buttons_visible` passes if any one of three buttons is visible;
    this is the witness. Both canonical ids on every app that carries them,
    each clicked, each read back through its ``copied`` attr (no driver reads
    the OS clipboard headlessly on linux/web). tui is short, honestly: it reuses
    the Account page's `account-actor-id-copy-btn` on its Status landing and
    has no node-URL copy — its declared per-app gap (`status.md` § Done
    definition), so it asserts that one button only.
    """
    app = logged_in_app
    if not (
        app.driver.is_tui() or app.driver.is_linux() or app.driver.is_web()
        or app.driver.is_macos() or app.driver.is_ios() or app.driver.is_windows()
    ):
        skip_unbuilt(
            app.driver,
            surface="the Status copy buttons' `copied` attribute",
            detail="the buttons copy, but only tui, linux, web, macos, ios and "
            "windows report the value they wrote, so a test cannot assert "
            "what reached the clipboard",
            tracked="",
        )
    app.settings._navigate_subpage("status")
    actor_id = app.settings.actor_id()
    assert len(actor_id) == 64, f"the Status page shows no actor id: {actor_id!r}"

    if app.driver.is_tui():
        copied = _copy_and_read_back(app, "account-actor-id-copy-btn")
        assert copied == actor_id, f"copied {copied!r}, the page shows {actor_id!r}"
        return

    copied = _copy_and_read_back(app, "status-actor-id-copy-btn")
    assert copied == actor_id, f"copied {copied!r}, the page shows {actor_id!r}"

    # The address the account is bound to. On web that is the page's own origin:
    # the e2e serves the SPA and reaches the nest through it, and web copies
    # the stored nest URL — the bound address, not the dialled socket.
    expected_url = nest_instance["url"]
    if app.driver.is_web():
        from urllib.parse import urlsplit

        spa = urlsplit(app.driver._spa_url or "")
        expected_url = f"{spa.scheme}://{spa.netloc}"
    copied_url = _copy_and_read_back(app, "status-node-url-copy-btn")
    assert copied_url.rstrip("/") == expected_url.rstrip("/"), (
        f"the node-URL copy put {copied_url!r} on the clipboard, but this "
        f"account's nest is {expected_url!r}"
    )


@pytest.mark.feature("status-quotas-and-limits")
def test_quota_section(logged_in_app):
    """The quota section renders with values fetched from `fauna.quota.get`.

    The section only renders once the quota fetch resolves (`{#if quota}` in
    settings/+page.svelte), so a broken transport leaves it hidden — this
    asserts the WS-RPC `fauna.quota.get` call actually returns the usage block.

    Navigates to the Status sub-page explicitly (`quota-section`'s canonical
    location on both macOS `MacStatusView` and iOS `StatusDetailView`) — see
    `test_actor_id_visible` for why a plain `.navigate()` doesn't reach it on
    iOS (its idiomatic settings nav lands on the root page list, not Status).
    """
    logged_in_app.settings._navigate_subpage("status")
    # The quota load is async (onMount → loadAll → fetchQuota); wait for the
    # section to appear rather than racing the render.
    try:
        logged_in_app.driver.wait_for("quota-section", timeout=10.0)
    except TimeoutError:
        raise AssertionError(
            "quota-section never rendered (fauna.quota.get may not have resolved): "
            f"{logged_in_app.driver.diagnose('quota-section')} "
            f"error={logged_in_app.error_text()!r}"
        ) from None
    assert logged_in_app.settings.is_quota_section_visible(), (
        "quota section should render after fauna.quota.get resolves "
        f"(error: {logged_in_app.error_text()!r})"
    )
    # At least one quota field must carry the fetched usage values.
    has_quota = (logged_in_app.settings.quota_inbox()
                 or logged_in_app.settings.quota_storage()
                 or logged_in_app.settings.quota_devices())
    assert has_quota, "Quota section visible but no quota values found"


def _status_text_once_loaded(app, element_id: str, *, unloaded=("", "—", "--")) -> str:
    """The Status page's ``element_id`` text once its async load has painted
    past the pre-load placeholder."""
    def _read():
        if not app.driver.is_visible(element_id):
            return None
        text = (app.driver.get_text(element_id) or "").strip()
        return None if text in unloaded else text

    return wait_until(
        _read,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"{element_id} never painted a loaded value; "
            f"{app.driver.diagnose(element_id)} error={app.error_text()!r}"
        ),
    )


# The Status page's sections 5–8 (`status.md` § Layout & flow) are built on a
# subset of apps today — the per-app matrix is the table there. tui, the lead
# app, renders all four from the shared snapshot (2026-09-27); macOS and iOS
# followed (2026-09-29); linux, web, windows and android trickle down.
_STATUS_DETAIL_TRACKED = ""


@pytest.mark.feature("status-quotas-and-limits")
def test_status_names_your_nest_and_its_version(logged_in_app, nest_instance):
    """`status.md` § Layout & flow: the Status page names the nest you are on
    and its version — the same values the nest itself publishes on
    ``fauna.nest.info``, not a placeholder."""
    app = logged_in_app
    if not (
        app.driver.is_linux()
        or app.driver.is_macos()
        or app.driver.is_ios()
        or app.driver.is_tui()
    ):
        skip_unbuilt(
            app.driver,
            surface="status-node-domain / status-node-version",
            detail="the Status page's node-info section is built on linux, macos, ios and tui only",
            tracked=_STATUS_DETAIL_TRACKED,
        )
    from tests.api import ws_api

    info = ws_api.nest_info(nest_instance["port"])
    app.settings._navigate_subpage("status")
    domain = _status_text_once_loaded(app, "status-node-domain")
    version = _status_text_once_loaded(app, "status-node-version")
    assert domain == info["domain"], (
        f"the Status page names the nest {domain!r}, but it is {info['domain']!r}"
    )
    assert version == info["version"], (
        f"the Status page names nest version {version!r}, but it runs {info['version']!r}"
    )


# The two sync witnesses read the LOCAL sync agent's answer, so the run needs a real
# agent on its own pipe: macOS spawns one only under `real_sync_agent`, and pairing
# it with `isolated_sync_agent` keeps windows on the per-session pipe it already
# defaults to (linux and tui spawn their own child and ignore both markers).
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
@pytest.mark.feature("status-quotas-and-limits")
def test_status_says_when_your_last_sync_pass_finished(logged_in_app):
    """`status.md` § Layout & flow: the Status page's sync section says when
    the last sync pass finished. A fresh account has never synced a file, so
    the honest reading is the shared "Never" — not a blank, not a raw key."""
    app = logged_in_app
    if not (
        app.driver.is_linux()
        or app.driver.is_windows()
        or app.driver.is_macos()
        or app.driver.is_tui()
    ):
        skip_unbuilt(
            app.driver,
            surface="status-sync-last",
            detail="the Status page's sync section is built on linux, windows, macos and tui only "
            "(ios has no local sync agent)",
            tracked=_STATUS_DETAIL_TRACKED,
        )
    app.settings._navigate_subpage("status")
    last = _status_text_once_loaded(app, "status-sync-last", unloaded=("", "--"))
    assert last == S.common.never, (
        f"a fresh account has never synced, but the Status page says {last!r}"
    )


@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
@pytest.mark.feature("status-quotas-and-limits")
def test_status_shows_your_pending_sync_backlog(logged_in_app):
    """`status.md` § Layout & flow: the sync section shows what this device
    still has pending — the local sync agent's backlog (`SyncStatusInfo`'s
    ``files_pending``/``bytes_pending``, `sync-agent.md` § Local agent health).
    A fresh device has nothing pending."""
    app = logged_in_app
    if not (app.driver.is_windows() or app.driver.is_macos() or app.driver.is_tui()):
        skip_unbuilt(
            app.driver,
            surface="status-sync-pending",
            detail="only windows, macos and tui read the local sync agent's backlog onto "
            "their Status page (linux's sync rows are the nest-side summary)",
            tracked=_STATUS_DETAIL_TRACKED,
        )
    app.settings._navigate_subpage("status")
    pending = _status_text_once_loaded(app, "status-sync-pending", unloaded=("",))
    assert pending.split()[0] == "0", (
        f"a fresh device has no sync backlog, but the Status page shows {pending!r}"
    )


@pytest.mark.feature("status-quotas-and-limits")
def test_status_shows_your_encryption_state_read_only(logged_in_app):
    """`status.md` § Encryption / MLS key packages: the Status page shows the
    MLS detail read-only — key packages published and secure channels open, as
    counts."""
    app = logged_in_app
    # windows draws the two MLS rows (`KeyPackageCountText`/`DmChannelCountText`) but
    # never fills them and carries no `status-mls-*` ids (`status.md` footnote ⁹) —
    # unbuilt until the shared-snapshot lift, not built.
    if not (app.driver.is_macos() or app.driver.is_ios() or app.driver.is_tui()):
        skip_unbuilt(
            app.driver,
            surface="status-mls-key-packages / status-mls-channels",
            detail="the Status page's read-only MLS detail is built on macos, ios and tui only "
            "(windows' rows are unfilled placeholders)",
            tracked=_STATUS_DETAIL_TRACKED,
        )
    app.settings._navigate_subpage("status")
    packages = _status_text_once_loaded(app, "status-mls-key-packages")
    channels = _status_text_once_loaded(app, "status-mls-channels")
    assert packages.isdigit(), f"the key-package count reads {packages!r}"
    assert channels.isdigit(), f"the secure-channel count reads {channels!r}"


@pytest.mark.feature("status-quotas-and-limits")
def test_status_names_the_build_you_are_running(logged_in_app):
    """`status.md` § Build: the page names the build you are running — a
    commit of this repository, abbreviated, so a user can check it against a
    published release."""
    import re
    import subprocess
    from pathlib import Path

    app = logged_in_app
    if not (
        app.driver.is_web()
        or app.driver.is_macos()
        or app.driver.is_ios()
        or app.driver.is_tui()
    ):
        skip_unbuilt(
            app.driver,
            surface="status-build-sha",
            detail="only web, macos, ios and tui name their build commit on the Status page",
            tracked=_STATUS_DETAIL_TRACKED,
        )
    app.settings._navigate_subpage("status")
    sha = _status_text_once_loaded(app, "status-build-sha")
    assert re.fullmatch(r"[0-9a-f]{12}", sha), (
        f"the Status page names build {sha!r}, not an abbreviated commit"
    )
    # The e2e build is of some commit of this checkout (possibly an earlier,
    # still-fresh build) — it must resolve here, not be a made-up string.
    repo = Path(__file__).resolve().parents[3]
    known = subprocess.run(
        ["git", "-C", str(repo), "cat-file", "-e", f"{sha}^{{commit}}"],
        capture_output=True,
    )
    assert known.returncode == 0, f"build {sha!r} is not a commit of this repository"


@pytest.mark.parametrize("page_id,expected_title", [
    ("account", S.settings.account_page.title),
    ("privacy", S.settings.privacy_page.title),
    ("encryption", S.settings.encryption_page.title),
])
def test_sub_page_heading_is_visible(logged_in_app, page_id, expected_title):
    """`global.elements` requires `page-heading` on EVERY authenticated page
    (ui.yaml, "in addition to each page's own elements") — a real, visible
    title, not just an id somewhere on the page (settings.md § Sub-page
    heading conformance).

    Guards the class of bug closed 2026-08-22: iOS's
    Account/Privacy/Encryption sub-pages had a real, visible native nav-bar
    title but carried no `page-heading` accessibility id (a bare
    `.navigationTitle(...)` instead of the shared FaunaKit `.pageTitle(...)`
    modifier), so a sighted user saw the title but no automated/accessibility
    tooling could find it.
    """
    logged_in_app.settings._navigate_subpage(page_id)
    logged_in_app.driver.wait_for("page-heading", timeout=10.0)
    heading = logged_in_app.driver.get_text("page-heading")
    assert heading == expected_title, (
        f"{page_id} page-heading should read {expected_title!r}, got {heading!r}: "
        f"{logged_in_app.driver.diagnose('page-heading')}"
    )


def test_general_sub_page_heading_is_visible(logged_in_app):
    """The `general` sub-page's title text deliberately varies by app (settings.md
    § Sub-page heading conformance: iOS renders Notifications there, a documented
    different feature, not the desktop apps' appearance/theme General page) — so
    this only asserts a real, non-empty, id-bearing heading paints, not a specific
    string. See `test_sub_page_heading_is_visible` for the uniform-title siblings.
    """
    if logged_in_app.driver.is_android():
        skip_unbuilt(
            logged_in_app.driver,
            surface="the general Settings sub-page",
            detail="not built yet on android (settings.md § Sub-page heading conformance table)",
            tracked="settings.md § Sub-page heading conformance",
        )
    logged_in_app.settings._navigate_subpage("general")
    logged_in_app.driver.wait_for("page-heading", timeout=10.0)
    heading = logged_in_app.driver.get_text("page-heading")
    assert heading, (
        f"general page-heading should render non-empty text, got {heading!r}: "
        f"{logged_in_app.driver.diagnose('page-heading')}"
    )
    # Non-empty is not enough on its own, and this test spent four whole-suite
    # sweeps proving it: linux answered this read with the FEED list's heading,
    # and 'Feeds' is non-empty, so the assertion above passed VACUOUSLY while
    # its three uniform-title siblings failed on the same bug. The title varies by app, but
    # whatever an app calls its General sub-page, it is not some OTHER page's
    # title — which is assertable without pinning the string.
    foreign_titles = {
        S.settings.account_page.title,
        S.settings.privacy_page.title,
        S.settings.encryption_page.title,
        S.feed.list.title,
    }
    assert heading not in foreign_titles, (
        f"general page-heading read {heading!r} — that is another page's title, "
        f"so this read resolved against the wrong page rather than the General "
        f"sub-page: {logged_in_app.driver.diagnose('page-heading')}"
    )
