"""The one answer to *"has onboarding landed in the authenticated app?"*.

Every journey that drives onboarding to its end needs the same check, and until
2026-09-01 five test files each carried their own copy of the marker tuple and
its poll loop (`test_trust_prompt.py`, `test_mail_enable_at_admin_claim.py`,
`test_mail_auto_enable_first_setup.py`,
`test_onboarding_logged_in_terminal_empty_store.py`, and — scoped to linux+tui —
`test_box_recovery_two_nest.py`). Four of them agreed; the fifth had drifted, and
the drift was invisible until it cost a session.

⚠ **The drift that motivated this module, kept here because the shape recurs.**
`test_trust_prompt.py`'s copy read `("feed-tab", "settings-tab",
"main-tab-view")` — a set with **no windows-workable member at all**, so both of
its answer-the-offer journeys hung for the full 120 s budget on an app that had
in fact landed. Two independent
reasons, and each alone is enough:

* `main-tab-view` is **iOS-only** (`ui.yaml` `global.platform_elements.ios`), so
  it never matches anywhere else.
* `feed-tab` / `settings-tab` are `NavigationView` entries, and on windows a
  revealed nav tab is **in the UIA tree but its rect is offscreen** — precisely
  what `WindowsBridgeDriver.is_nav_tab_revealed` documents and why that override
  exists. `is_visible()` tests `!IsOffscreen`, so it answers False for a nav tab
  that is present and revealed.

The lesson generalizes past windows: **a nav entry is not a landing landmark.**
Reach for the landed page's own content (`feed-view`) and let the nav entries be
the corroboration they are.
"""
from helpers.budgets import APP_RELAUNCH_S
from helpers.waiting import wait_until

#: Every landmark that means "the authenticated shell has mounted", across all
#: seven apps. Deliberately a UNION and deliberately generous: each journey that
#: polls it only needs *onboarding is done*, never *which* surface the app chose
#: to land on — and apps legitimately differ there (linux/tui/web/windows/android
#: land on the feed; macOS defaults to Conversations; iOS paints its root
#: TabView). A caller wanting a specific landing asserts that separately, after
#: this returns.
SHELL_MARKERS = (
    # The feed page's own content. The only member windows can see, since its
    # nav entries render offscreen (see the module docstring) — do not drop it.
    "feed-view",
    # Nav entries: real landmarks on the apps whose tabs render on-screen.
    "feed-tab",
    "settings-tab",
    # macOS defaults to Conversations rather than the feed.
    "new-conversation-button",
    "conversation-search-box",
    # iOS's root TabView container (`ui.yaml`: ios-scoped).
    "main-tab-view",
)


def describe_markers(app, markers=SHELL_MARKERS) -> str:
    """Per-marker `visible=…, count=…` snapshot for a failed landing.

    This is the convention-6 half of the same lesson. The diagnosis this
    replaces called `driver.tree()`, which **only the apple bridge implements** —
    every other bridge returns `""` by contract, so on six of seven apps the
    failure message carried the empty string and said nothing at all.

    `PlatformDriver.diagnose` is cross-platform and reports `visible` *and*
    `count` separately, which is exactly the distinction that names this class of
    bug on sight: `visible=False, count=1` is "rendered, but its rect is
    offscreen" (a marker-choice bug), while `visible=False, count=0` is "never
    rendered" (the app really did not land).
    """
    return " ".join(app.driver.diagnose(m) for m in markers)


def wait_for_authenticated_shell(app, budget_s: float = APP_RELAUNCH_S,
                                 markers=SHELL_MARKERS) -> str:
    """Wait for the authenticated shell and return the marker that appeared.

    A deadline poll on the shared primitive (convention 14): the budget is a
    ceiling a green run never pays, not a settle time. `APP_RELAUNCH_S` is the
    right default — what is waited on is the post-claim launch, an app start.

    A post-claim launch can surface `launch-retry-button` first; clearing it is
    part of the wait, not a separate step the caller has to remember.
    """

    def arrived():
        for marker in markers:
            if app.driver.is_visible(marker):
                return marker
        if app.driver.is_visible("launch-retry-button"):
            app.driver.click("launch-retry-button")
        return None

    return wait_until(
        arrived,
        budget_s,
        diagnose=lambda: "onboarding never reached the authenticated app. "
        f"error: {app.error_text()!r} {describe_markers(app, markers)}",
    )
