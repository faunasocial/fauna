r"""tier_1: the iOS driver reads the app's own log, so an iOS failure diagnoses itself.

E2E convention 6 (`docs/goal/architecture/e2e-conventions.md` § point 6): "don't
debug with screenshots — failures must diagnose themselves." Every other native
driver family already keeps the client's own account of itself — windows and the
linux/tui pair on disk or as captured stderr, macOS as its launch's `app.err` —
and `helpers/app_log_section.py` attaches whichever it finds to every failing
test's report. **iOS had neither attribute**, so an iOS failure reported a bare
timeout with no trace of what the app thought it was doing.

That hole stalled a real track: the W3 (account-data-plane.md § Workstreams) account-runtime acceptance test's own
failure message routes the reader to "grep the app log for `account runtime:`" to
tell *assembly failed* from *superseded* from *refused at the door* from *the hook
never fired*, and on iOS there was nothing to grep — two ~12-minute runs produced
the same unreadable `role: (False, False)`.

These are tier_1 on purpose. The reader is a pure function of the resolved app
data container, so what it returns can be pinned against a fabricated container
in milliseconds — no simulator, no app build, no nest. That is the mile/inch split
the "needs a real run" claim would have hidden: only *which log line is present*
needs a real iOS run; *whether the driver can read the log at all* does not.
"""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers.ios import IosInProcessDriver  # noqa: E402
from helpers import app_log_section  # noqa: E402

# tier_1 only, deliberately NO `ios` app marker: this needs no simulator and no app
# build, so gating it behind `--app ios` would hide a cross-app harness contract
# behind the one run nobody does by default. Same call `test_tui_driver_env.py`
# makes for the tui driver's env contract.
pytestmark = pytest.mark.tier_1


def _driver_on(app_support: Path | None) -> IosInProcessDriver:
    """A driver whose launch already resolved (or failed to resolve) the container.

    `launch()` sets `_app_support` from `simctl get_app_container` right after the
    install; everything below is what happens afterwards, so pinning the attribute
    is the whole of the driver state this reader consumes.
    """
    driver = IosInProcessDriver()
    driver._app_support = str(app_support) if app_support is not None else None
    return driver


def test_reads_the_rolling_log_the_app_actually_writes(tmp_path):
    """`<Application Support>/logs/fauna.log.<date>` — the shared `fauna_log` path.

    `FaunaApp.init()` hands `.applicationSupportDirectory` to `installLogging`, and
    `fauna_log::native::init` writes `<data_dir>/logs/fauna.log.<date>`. This pins
    the join between those two, which is the only thing that makes the file
    findable from the host.
    """
    logs = tmp_path / "logs"
    logs.mkdir()
    (logs / "fauna.log.2026-08-26").write_text("account runtime: assembly failed\n")

    assert "account runtime: assembly failed" in _driver_on(tmp_path).app_log_text()


def test_a_rolled_log_is_read_whole_and_in_date_order(tmp_path):
    """The appender rolls DAILY, so a run spanning midnight leaves two files.

    Reading only the newest would drop exactly the startup lines a post-mortem
    wants (the app launched yesterday, the failure is today). Sorted-glob keeps
    both, oldest first — the same contract windows' twin holds.
    """
    logs = tmp_path / "logs"
    logs.mkdir()
    (logs / "fauna.log.2026-08-25").write_text("first day\n")
    (logs / "fauna.log.2026-08-26").write_text("second day\n")

    text = _driver_on(tmp_path).app_log_text()
    assert text.index("first day") < text.index("second day")


def test_an_unresolved_container_returns_none_not_an_exception(tmp_path):
    """`_resolve_app_support` is best-effort and leaves `None` when simctl fails.

    A diagnostic must never mask the failure it exists to explain, so the reader
    answers `None` rather than raising — `app_log_section` turns that into its
    explicit `<empty>` verdict.
    """
    assert _driver_on(None).app_log_text() is None


def test_a_container_with_no_logs_dir_returns_none(tmp_path):
    """An app that never reached `installLogging` writes no `logs/` at all.

    Distinguishable from a present-but-silent log only because the section
    renders `<empty>` either way *and says so* — what must not happen here is an
    exception out of `Path.glob` on a missing directory.
    """
    assert _driver_on(tmp_path).app_log_text() is None


def test_an_unreadable_file_is_skipped_rather_than_losing_the_whole_log(tmp_path):
    """One bad file must not cost the other file's lines.

    A directory named like a log file is the cheap portable stand-in for the
    `OSError` a permissions or reclaim race produces; either way the reader keeps
    going, exactly as windows' twin does.
    """
    logs = tmp_path / "logs"
    logs.mkdir()
    (logs / "fauna.log.2026-08-25").mkdir()  # reads as a directory -> OSError
    (logs / "fauna.log.2026-08-26").write_text("survived\n")

    assert "survived" in _driver_on(tmp_path).app_log_text()


def test_the_shared_report_section_now_finds_ios(tmp_path):
    """The point of the attribute: the GENERIC surfacing path picks iOS up.

    `app_log_section._TEXT_ATTRS` tries `app_log_text` first, so no test needs its
    own reader — this is the assertion that iOS joined the other native families
    rather than growing a fourth bespoke diagnoser.
    """
    logs = tmp_path / "logs"
    logs.mkdir()
    (logs / "fauna.log.2026-08-26").write_text("account runtime: started\n")

    title, body = app_log_section.build(_driver_on(tmp_path))
    assert title == "app log (app_log_text)"
    assert "account runtime: started" in body


def test_no_container_still_yields_the_explicit_empty_verdict(tmp_path):
    """`<empty>` is evidence, not silence — see `app_log_section`'s three answers.

    iOS must land in the "there is a reader and it returned nothing" bucket, never
    back in the "this family keeps no log" bucket it was in before.
    """
    section = app_log_section.build(_driver_on(None))
    assert section is not None
    title, body = section
    assert title == "app log (app_log_text)"
    assert "<empty" in body
