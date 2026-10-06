"""Unit tests for the aftermath journey's failure diagnostics (tier_1).

`test_identity_succession_aftermath.py`'s `diagnose=` closures are the only
thing a failing run leaves behind, and they are **structurally untestable by the
journey itself**: they run only when an assertion fails, so a green tier_3 run
proves nothing about them and a red one has already cost the session it was
supposed to help. That asymmetry is how the bug this file pins survived — a
diagnostic quoted a Settings progress line while the app sat on Backups, so it
reported `''` unconditionally, and `''` on that line reads exactly like *"the leg
never ran"*. The session took it as its one discriminator and spent a run
aimed at the wrong candidate while the app log for that very run showed both legs
settling (`e2e-conventions.md` § point 6 — a failure must diagnose itself).

So the readers are pinned here instead, against a stand-in app whose page state
behaves the way the real one does: an `is_visible`-backed reader answers for its
own page and empty for every other. That is the whole mechanism, and it is
testable in milliseconds without a nest.
"""

import pathlib

import pytest

pytestmark = pytest.mark.tier_1

from helpers.diagnostics import aftermath_log_lines, backup_regrant_line, settings_line


class _Settings:
    """The Settings surface, with the page-scoping that made the bug invisible.

    `aftermath_*_status` are `is_visible` probes in the real driver: they
    navigate nowhere, and an element on another page is simply not visible — so
    they answer `""` rather than raising. Reproduced faithfully, because a
    stand-in that raised instead would make the bug impossible to write.
    """

    def __init__(self, page: str = "backups") -> None:
        self.page = page
        self.opened = 0

    def open_recovery_kit(self) -> None:
        self.page = "settings"
        self.opened += 1

    def aftermath_backup_regrant_status(self) -> str:
        return "Re-granted" if self.page == "settings" else ""

    def explodes(self) -> str:
        raise RuntimeError("driver went away")


class _App:
    def __init__(self, settings: _Settings) -> None:
        self.settings = settings


def test_a_quoted_settings_line_is_read_on_the_page_it_lives_on():
    """The fix: go to the page, then quote it.

    The app starts on Backups — where every one of these diagnostics actually
    runs, because the poll body that just failed left it there.
    """
    settings = _Settings(page="backups")
    app = _App(settings)

    # The bug, stated as the control: the bare read the diagnostics used to do.
    assert settings.aftermath_backup_regrant_status() == "", (
        "precondition — a Settings reader answers empty from another page, "
        "which is what made a broken diagnostic look like a settled fact"
    )

    assert backup_regrant_line(app) == "Re-granted", (
        "the helper must navigate before reading, or the diagnostic can only "
        "ever report '' — indistinguishable from 'that leg never ran'"
    )
    assert settings.opened == 1, "and it navigates exactly once per quote"


def test_an_unreadable_line_says_so_instead_of_passing_as_empty():
    """A failure inside the diagnostic must not become a fact about the product.

    Returning `""` here would recreate the original bug one layer down: the
    reader never answered, and the message would claim the leg is unstarted.
    """
    app = _App(_Settings())

    line = settings_line(app, "explodes")

    assert line == "<unreadable: RuntimeError>", line
    assert line != "", "an unread line must never be reported as an empty line"


def test_a_missing_reader_is_reported_rather_than_raised():
    """A diagnostic runs on the failure path and must never raise out of it —
    a renamed reader would otherwise replace the real assertion error with an
    `AttributeError` from the message that was supposed to explain it."""
    app = _App(_Settings())

    assert settings_line(app, "no_such_reader") == "<unreadable: AttributeError>"


# ── `aftermath_log_lines` — the witness that survives an unpainted render ─────
#
# The `*_line` readers above quote RENDERED progress lines, so they answer `""`
# on every shell that paints none — and, on any shell, for a leg whose settled
# arm is deliberately unpainted (`ReplicaResealOutcome::NothingStored` and
# `AlreadyCurrent` both return `None` from `settled_line`, because a line
# announcing a no-op at every later sign-in trains the user past the one that
# matters). `aftermath_log_lines` is what separates those, and the two pins
# below cover the two ways it was blind.

class _Driver:
    """A driver exposing exactly ONE log surface — the capability dispatch is
    the thing under test, so each stand-in must offer only its own."""

    def __init__(self, *, stderr: str | None = None, console: list[str] | None = None):
        if stderr is not None:
            self.app_stderr_text = lambda: stderr
        if console is not None:
            self.console_log = lambda: list(console)


class _LoggingApp:
    def __init__(self, driver: _Driver) -> None:
        self.driver = driver


# The browser-console mirror's exact wire format — `[LEVEL] target: message`
# (`libs/fauna-wasm/src/logs.rs`'s `ConsoleLayer::on_event`). Written out rather
# than paraphrased: this reader greps target and message text, so a stand-in
# that invented its own shape would pin nothing about the real one.
_LEG3_CONSOLE = (
    "[INFO] fauna_client_mls_sync::sync: the __mls replica re-seal pass "
    "settled at load outcome=NothingStored"
)
_LEG7_CONSOLE = (
    "[INFO] fauna_client_recovery::aftermath: the __drafts re-seal pass settled "
    "outcome=Resealed { rails: 1 }"
)
# Leg 3's inputs, logged before they collapse into a fieldless outcome
# (`fauna_client_mls_sync::succession`). `channels=0` is the reading that makes
# a "the conversations came back" assertion vacuous.
_LEG3_EXAMINED = (
    "[INFO] fauna_client_mls_sync::succession: the __mls re-seal pass examined "
    "the replica provider_ours=true channels=0 histories=0 owed=0"
)


def test_the_browser_console_is_an_app_log_too():
    """Web keeps its log on the console, and that used to read as *no* log.

    The reader dispatches by capability, never by driver type (convention 3),
    but it knew only the native shells' `app_stderr_text` — so on web it
    answered `<no app-log reader on this driver>` and every aftermath journey
    there lost the one witness that survives an unpainted line. Web's wasm
    subscriber mirrors every `tracing` event to the console
    (`logs.rs` § Browser-console layer), so the witness was always present;
    only the reader could not see it.
    """
    app = _LoggingApp(_Driver(console=[_LEG7_CONSOLE, "[INFO] unrelated: noise"]))

    out = aftermath_log_lines(app)

    assert "the __drafts re-seal pass settled" in out, out
    assert "no app-log reader" not in out, (
        "web has a log surface; reporting it as absent is what cost the leg-3 "
        "investigation its discriminator"
    )
    assert "unrelated: noise" not in out, "only aftermath lines belong in this quote"


def test_leg_3_is_quoted_even_though_it_logs_from_another_crate():
    """Leg 3's verdict line was missed on EVERY app, not just web.

    It is the one leg that does not report through `run_succession_aftermath`
    (it is a barrier inside the replica's own `load()`), so it also logs from a
    different target — `fauna_client_mls_sync::sync`, which matches none of the
    aftermath markers. Its arm is exactly the one a reader most needs: an empty
    leg-3 line means `NothingStored`, `AlreadyCurrent`, *or* a pass that never
    ran, and only this line tells them apart.
    """
    app = _LoggingApp(_Driver(console=[_LEG3_CONSOLE]))

    out = aftermath_log_lines(app)

    assert "NothingStored" in out, (
        f"leg 3's settled arm must survive into the quote; got {out!r}"
    )


def test_a_driver_with_no_log_surface_still_says_so():
    """An absent witness and a silent one must not read alike — the property
    the web arm above must not have broken."""
    app = _LoggingApp(_Driver())

    assert aftermath_log_lines(app) == "<no app-log reader on this driver>"


def test_a_reader_that_raises_never_replaces_the_real_failure():
    """A diagnostic runs on the failure path; a fault here must be reported as
    a fault, not raised over the assertion error it was meant to explain."""

    driver = _Driver()
    def _boom() -> list[str]:
        raise RuntimeError("bridge went away")
    driver.console_log = _boom
    app = _LoggingApp(driver)

    out = aftermath_log_lines(app)

    assert out.startswith("<unreadable: RuntimeError"), out


# ── The console ring's own truncation ───────────────────────────────────────
#
# The three seams that carry "this log starts mid-stream" from the bridge to a
# failure message. A bounded ring that forgets its head silently turns "the line
# is not here" into "the thing never happened" — and a session spent a whole
# pass on that inversion: a succession journey boots the SPA twice (the ceremony
# ends in a hard `window.location.assign`), the two boots together overrun the
# 500-line ring, the first boot's evidence scrolls off, and the surviving
# `grep -c` of 1 reads like proof the pass ran exactly once.
# 

_EVICTION_WARNING = (
    "<⚠ 7 earlier console line(s) EVICTED from the 500-entry ring — this log "
    "starts mid-stream, so a line's absence here is NOT evidence it was never "
    "logged>"
)


def test_an_evicted_ring_says_so_even_though_it_is_not_an_aftermath_line():
    """The warning matches no aftermath marker, and must survive anyway.

    This reader's whole job is to let a caller conclude something from what is
    NOT in the log. A marker filter that drops the one line saying the log is
    incomplete would hand that caller a confident, unsound answer — which is
    exactly the failure this pin exists for.
    """
    app = _LoggingApp(_Driver(console=[_EVICTION_WARNING, _LEG3_CONSOLE]))

    out = aftermath_log_lines(app)

    assert "EVICTED" in out, (
        f"the truncation warning must survive the marker filter; got {out!r}"
    )
    assert "NothingStored" in out, out


def test_an_evicted_ring_with_no_aftermath_line_is_not_reported_as_absence():
    """`<no aftermath line in this launch's log>` is a claim about the app.

    When the ring dropped its head that claim is unsupportable, so the reader
    must hand back the truncation instead of the absence verdict.
    """
    app = _LoggingApp(_Driver(console=[_EVICTION_WARNING, "[INFO] unrelated: noise"]))

    out = aftermath_log_lines(app)

    assert "EVICTED" in out, out
    assert "no aftermath line" not in out, (
        "a truncated log cannot support the absence verdict — that is the "
        "inversion this whole pin exists to stop"
    )


def test_the_driver_turns_a_drop_count_into_a_leading_warning_line():
    """The bridge counts; the driver is what makes the count legible.

    Pinned through the real `WebDriver.console_log` (its only dependency is the
    one bridge call, stubbed here) so the wire key it reads — `dropped` — stays
    the key the bridge writes.
    """
    from drivers.web import WebBridgeDriver

    driver = object.__new__(WebBridgeDriver)
    driver._get = lambda route: {"lines": ["[console.log] a", "[console.log] b"], "dropped": 7}

    lines = driver.console_log()

    assert "EVICTED" in lines[0], f"the warning must lead, not hide in the tail: {lines!r}"
    assert "7" in lines[0], lines[0]
    assert lines[1:] == ["[console.log] a", "[console.log] b"], (
        f"the captured lines must be handed back untouched; got {lines[1:]!r}"
    )


def test_an_intact_ring_adds_no_warning():
    """A warning on every read would train the reader to ignore it — the same
    reason `ReplicaResealOutcome::settled_line` paints nothing for a no-op."""
    from drivers.web import WebBridgeDriver

    driver = object.__new__(WebBridgeDriver)
    driver._get = lambda route: {"lines": ["[console.log] a"], "dropped": 0}

    assert driver.console_log() == ["[console.log] a"]


def test_the_bridge_counts_what_the_ring_evicts():
    """The counter half, against the real bridge module.

    `_console_dropped` is what makes eviction observable at all; a ring that
    counted only what it still holds could never report a loss.
    """
    import importlib.util

    server_path = (
        pathlib.Path(__file__).resolve().parents[1] / "web-bridge" / "server.py"
    )
    spec = importlib.util.spec_from_file_location("fauna_e2e_web_bridge", server_path)
    bridge = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bridge)

    pid = "ring-pin"
    assert bridge._console_dropped(pid) == 0, "a ring nobody wrote to lost nothing"

    for i in range(bridge._CONSOLE_RING_MAX + 3):
        bridge._console_append(pid, f"[console.log] {i}")

    assert len(bridge._console_logs[pid]) == bridge._CONSOLE_RING_MAX
    assert bridge._console_dropped(pid) == 3, (
        "three lines past the bound were evicted and the count must say so"
    )
    assert "[console.log] 0" not in bridge._console_logs[pid], (
        "the deque evicts its HEAD — the oldest boot's evidence is what a "
        "multi-boot journey loses"
    )


def test_leg_3s_examined_line_is_kept_whole_not_left_to_the_tail():
    """The line that names `AlreadyCurrent`'s arm must not scroll off.

    Leg 3 logs its inputs (`provider_ours`, `channels`, …) and then SIX more
    legs log after it. The tail keeps the last few non-verdict lines, so a
    line that is merely "matched" is exactly the one a busy aftermath pushes
    out — and this one is the only thing separating "every slice was already
    ours" from "the provider carried no channels, so nothing was examined".
    """
    noise = [
        f"[INFO] fauna_client_recovery::aftermath: leg {i} settled" for i in range(20)
    ]
    app = _LoggingApp(_Driver(console=[_LEG3_EXAMINED] + noise))

    out = aftermath_log_lines(app, limit=3)

    assert "examined the replica" in out, (
        f"leg 3's inputs must survive the tail; got {out!r}"
    )
    assert "channels=0" in out, out
