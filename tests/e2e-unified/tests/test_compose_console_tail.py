"""Unit tests for the compose timeout's browser-console diagnostic (tier_1).

`FeedActions._compose_console_tail` is a **failure-only** surface: it runs
exactly when a compose assertion has already timed out, so a green tier_3 run
proves nothing about it and a red one has already spent the session it exists to
help. That is the same asymmetry `test_aftermath_diagnostics.py` was written for,
and the same reason this pin lives here rather than inside a journey.

The bug it pins is a mismatch between the rule and the failure. The tail rule —
carry the last `CONSOLE_TAIL_LINES` of the ring — is correct for the failure it
was written for: a submit silently dropped AT SUBMIT, whose evidence is the last
thing logged. It is wrong for the failure where the composer never became READY,
whose evidence is written during BOOT — the launch machine's routing, the feed
page's mount, the actor-scope pass — and is therefore the first thing to fall off
a 500-line ring reported 40 lines at a time. A reader then sees a timeout with no
boot evidence at all and cannot tell "the launch machine routed this session to
the wizard" from "everything booted and the composer is merely slow", which is
the discrimination the whole diagnostic exists to make.
"""

import pytest

pytestmark = pytest.mark.tier_1

from actions.feed import FeedActions


class _WebDriver:
    """A web driver exposing only the console surface the diagnostic reads."""

    def __init__(self, lines: list[str]) -> None:
        self._lines = lines

    def is_web(self) -> bool:
        return True

    def console_log(self) -> list[str]:
        return list(self._lines)


class _NativeDriver:
    """Every non-web app: no console at all."""

    def is_web(self) -> bool:
        return False


def _ring_with_boot_breadcrumbs_then_noise(noise: int) -> list[str]:
    """A ring shaped like the real failure: boot breadcrumbs first, then enough
    unrelated chatter to push them past the tail window."""
    boot = [
        "[debug] [launch] createLaunchMachine",
        "[debug] [launch] identity inputs: "
        '{"index":"parsed","active":"deadbeef","active_secret_row":"absent",'
        '"legacy_derives_active":false,"load_identity":"NONE"}',
        "[debug] [launch] applyLaunchPhase: WizardAt IdentityChoice",
        "[debug] [feed] mount: registering the actor-scope seam",
    ]
    return boot + [f"[log] unrelated chatter {i}" for i in range(noise)]


def test_boot_breadcrumbs_survive_the_tail_window():
    """The line that names WHY the launch machine chose the wizard is emitted at
    boot, so on a noisy page it sits outside the tail. It must still be carried:
    without it the failure text cannot distinguish the two states, which is the
    only reason it is being read."""
    noise = FeedActions.CONSOLE_TAIL_LINES * 3
    driver = _WebDriver(_ring_with_boot_breadcrumbs_then_noise(noise))

    out = FeedActions(driver)._compose_console_tail()

    # The boot evidence — outside the tail by construction — is present.
    assert "[launch] identity inputs:" in out, (
        "the launch machine's identity inputs fell off the diagnostic; "
        f"got:\n{out}"
    )
    assert "WizardAt IdentityChoice" in out
    assert "[feed] mount" in out
    # …and it is labelled as a rescue, not spliced silently into the tail.
    assert "boot breadcrumbs + errors rescued" in out
    # …and the tail itself is still what it was.
    assert f"unrelated chatter {noise - 1}" in out
    assert f"unrelated chatter {noise - FeedActions.CONSOLE_TAIL_LINES}" in out


def test_no_rescue_section_when_nothing_was_elided():
    """A short ring is entirely inside the tail, so there is nothing to rescue
    and no header claiming otherwise."""
    driver = _WebDriver(["[debug] [launch] createLaunchMachine", "[log] done"])

    out = FeedActions(driver)._compose_console_tail()

    assert "boot breadcrumbs + errors rescued" not in out
    assert "earlier line(s) elided" not in out
    assert "[launch] createLaunchMachine" in out


def test_rescue_is_capped_so_a_boot_loop_cannot_flood_the_message():
    """The page this diagnostic is aimed at LOOPS its boot, so the breadcrumbs
    are the one class that can outnumber the noise. The cap is what keeps a
    readable failure readable."""
    loops = FeedActions.CONSOLE_PROBE_MAX * 4
    ring = [f"[debug] [launch] cycle {i}" for i in range(loops)]
    ring += [f"[log] chatter {i}" for i in range(FeedActions.CONSOLE_TAIL_LINES * 2)]
    driver = _WebDriver(ring)

    out = FeedActions(driver)._compose_console_tail()

    rescued = [ln for ln in out.splitlines() if "[launch] cycle" in ln]
    assert len(rescued) == FeedActions.CONSOLE_PROBE_MAX, (
        f"expected the rescue to cap at {FeedActions.CONSOLE_PROBE_MAX}, got {len(rescued)}"
    )
    # The NEWEST cycles are the ones kept — a boot loop's last state is the one
    # that produced the failure being reported.
    assert f"[launch] cycle {loops - 1}" in out
    assert "older elided" in out


def test_a_driver_with_no_console_contributes_nothing():
    """Best-effort and never masking: every native app has no console, and the
    timeout it is decorating must still be the failure the reader sees."""
    assert FeedActions(_NativeDriver())._compose_console_tail() == ""


def test_pageerror_survives_the_tail_window():
    """An uncaught page error is rescued from the elided head like a breadcrumb.

    This is the witness class the breadcrumbs cannot stand in for. A
    `wasm_bindgen_futures` task whose poll throws a JS exception dies mid-poll:
    the wasm async fn's JS promise **never settles** — no resolution, no
    rejection — so every `await` on it hangs forever and every internal deadline
    is dead too, because the bound lived inside the killed task. The single
    witness it leaves is a browser `pageerror` (the bridge captures uncaught
    errors AND unhandled promise rejections, `web-bridge/server.py:151-158`).

    That is precisely the shape the composer failure is
    left with: the feed
    page's `getFeedManager()` neither resolved nor rejected, so `feedReady` and
    `loadError` are BOTH unset and the composer sits disabled for the full
    ceiling with an empty `error-message`. Nothing in the DOM separates it from
    a slow feed, and the breadcrumbs cannot either — they say the boot ran, not
    that a promise died. The `pageerror` is emitted when the task is polled,
    i.e. mid-journey and well before the timeout, so on a page that logs
    anything afterwards it falls off the tail exactly as the boot lines do.

    Rescuing it is what lets the next red name this cause instead of paying
    another run to ask.
    """
    noise = FeedActions.CONSOLE_TAIL_LINES * 3
    ring = (
        ["[pageerror] TypeError: Cannot read properties of undefined"]
        + [f"[log] unrelated chatter {i}" for i in range(noise)]
    )
    driver = _WebDriver(ring)

    out = FeedActions(driver)._compose_console_tail()

    assert "[pageerror]" in out, (
        "an uncaught page error fell off the diagnostic — it is the ONLY "
        "witness a wasm future that died mid-poll leaves behind, and the "
        f"failure it explains is silent everywhere else; got:\n{out}"
    )
    assert "TypeError" in out
    # Rescued, not silently spliced into the tail.
    assert "rescued" in out


def test_singleton_build_stall_survives_the_tail_window():
    """The SPA's own settle-deadline report is rescued, and it names the builder.

    `[pageerror]` above is the witness for a build that died WITHOUT the app
    noticing. This is the complement: the app noticed. `$lib/singleton-build.ts`
    wraps every memoized actor-scoped build in an external settle deadline —
    external because a deadline living inside a dead wasm task dies with it —
    and on expiry it rejects, clears the memo, and writes one `[singleton]` line
    naming which build never reached a terminal state.

    That line is the difference between "the composer is disabled and nothing
    anywhere says why" and a located
    failure, because five builders share the symptom: a dead WS-RPC client, a
    dead feed manager and a dead event-drafts face all present as a surface that
    silently never becomes ready. It is written ~45 s before the 90 s compose
    ceiling, so it is subject to the same tail-window loss as every other boot
    line — which is the whole reason this test exists rather than a comment.
    """
    noise = FeedActions.CONSOLE_TAIL_LINES * 3
    ring = (
        [
            "[error] [singleton] feed manager build failed: SingletonBuildStalled: "
            "feed manager: build neither resolved nor rejected within 45s"
        ]
        + [f"[log] unrelated chatter {i}" for i in range(noise)]
    )
    driver = _WebDriver(ring)

    out = FeedActions(driver)._compose_console_tail()

    assert "[singleton]" in out, (
        "the SPA's own settle-deadline report fell off the diagnostic — it is "
        "the one line that says WHICH singleton build never reached a terminal "
        f"state, and five builders share the symptom; got:\n{out}"
    )
    assert "feed manager" in out, "the rescued line must still name the builder"
    assert "rescued" in out
