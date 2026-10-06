from __future__ import annotations

import atexit
import json
import os
import shutil
import subprocess
import sys
import tempfile
import re
import time
import weakref
from pathlib import Path

from .base import detached_agent_log_text, stitch_agent_log
from .http_bridge import HttpBridgeDriver
from .port_util import drain_pipes, popen_group_kwargs, reap_descendants_of

# An ordinary tracing line from the app: `2026-09-01T16:03:35.432106Z  INFO …`.
# Used only to decide where a multi-line fatal stack trace STOPS.
_APP_LOG_LINE_RE = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}")

# The launch-collision chooser's own UIA anchor (`ui.yaml`'s
# `launch-instance-chooser`, `LaunchInstanceChooserPage.xaml`). A launch that
# collides lands here — `App.DetectLaunchCollision` /
# `ShowLaunchInstanceChooserAsync` — BEFORE `Testing.TestAgent.Start` runs in
# `App.xaml.cs`, by design: a stranded instance must never read or serve the
# already-served account's state. So `/app/state` never turns truthy for such
# a launch, and the readiness gate in `launch()` below must accept this
# anchor's presence as an equally valid "ready" signal, read straight off the
# FlaUI bridge's own UIA tree (never TestAgent) — same duplication as the two
# `CHOOSER_ANCHOR` test-file constants; this driver has no generated-ID import
# of its own to share it with.
_LAUNCH_CHOOSER_ANCHOR_ID = "launch-instance-chooser"

#: The launch argument that makes a test-agent FaunaApp build show its window
#: without activating it (`App.xaml.cs`'s `E2eNoActivateArg`; e2e convention 10's
#: windows focus axis). Passed on every launch unless the config says
#: `activate_window=True`.
NO_ACTIVATE_ARG = "--e2e-no-activate"

_BRIDGE_DIR = Path(__file__).parent.parent / "flaui-bridge"
_BRIDGE_EXE = _BRIDGE_DIR / "bin" / "Debug" / "net10.0-windows" / "FauiBridge.exe"
# drivers/ → e2e-unified/ → tests/ → repo root (where the justfile lives).
_REPO_ROOT = _BRIDGE_DIR.parents[2]
# The bare `cargo build -p fauna-sync-agent` output — pinned so the app's
# candidate probes (`fauna_client_sync::agent_spawner::windows_agent_candidates`
# — beside FaunaApp.exe, one level up, %ProgramFiles%\Fauna) never have to find
# it on their own. Unlike linux/macOS, windows has no build/packaging step that
# co-locates the two binaries (macOS's `mac-debug` copies the cargo-built agent
# into the .app bundle; linux builds them side by side by construction). A pin
# that misses spawns NOTHING rather than silently falling through to a stray
# machine-installed agent (AGENT_BIN_ENV's own contract) — deliberately not
# conditional on the path existing, matching FAUNA_E2E_RESCAN_MS below.
_SYNC_AGENT_EXE = _REPO_ROOT / "target" / "debug" / "fauna-sync-agent.exe"


def _default_sync_agent_pin(env: dict) -> None:
    """Pin the dev agent unless the caller already chose, and an EMPTY value is a
    choice: the contract's own "not pinned" (Rust `pinned_agent_binary`, C#
    `E2eEnv.SyncAgentBin` via `IsNullOrEmpty`). conftest's
    `_apply_installed_product_agent_env` writes it so the MSI-installed app, built
    with the test-flavored FFI and therefore honouring the pin, spawns its OWN
    installed agent rather than this dev tree's."""
    env.setdefault("FAUNA_E2E_SYNC_AGENT_BIN", str(_SYNC_AGENT_EXE))

# Track all live bridge processes so atexit can clean them up.
# Uses weak references so drivers that are properly teardown'd don't accumulate.
_live_bridges: list[weakref.ref] = []

# Exit codes worth NAMING in an ack-timeout post-mortem, keyed by their unsigned
# 32-bit form. A windows app that dies mid-command reports one of these far more
# often than anything else, and the hex spelling is the form a reader can look up.
_EXIT_CODE_NOTES: dict[int, str] = {
    0xC0000409: (
        "STATUS_STACK_BUFFER_OVERRUN — .NET fail-fast, or a Rust `abort()`; "
        "UniFFI aborts the process on a panic crossing the FFI boundary"
    ),
    0xC000027B: (
        "STATUS_STOWED_EXCEPTION — an unhandled exception inside a WinRT/XAML "
        "callback; the MEASURED signature of this app crashing on this machine "
        "(faulting module Microsoft.UI.Xaml.dll / CoreMessagingXP.dll). Managed "
        "or WinRT code, NOT a Rust panic"
    ),
    0xC0000005: "STATUS_ACCESS_VIOLATION",
    0xC0000602: "STATUS_FAIL_FAST_EXCEPTION — an explicit fail-fast",
    0xC000013A: "STATUS_CONTROL_C_EXIT — the process was signalled, not crashed",
    0xE0434352: "an unhandled .NET exception (CLR)",
    0x80000003: "STATUS_BREAKPOINT — a debug break / fail-fast assertion",
    0x00000000: (
        "ZERO — the app exited CLEANLY. It was not killed and did not crash: it "
        "QUIT. Suspect a UI path that tears the app down (a wizard terminal "
        "calling window.close()); no WER record is written for this, which is "
        "how it masquerades as a crash"
    ),
}


def _atexit_cleanup():
    """Kill any bridge processes still alive when Python exits."""
    for ref in _live_bridges:
        driver = ref()
        if driver is not None:
            driver._force_kill()


atexit.register(_atexit_cleanup)


# How long `just windows-flaui-bridge` gets. Named and generous (convention 14),
# because the thing being waited on is NOT a compile — it is a QUEUE. The recipe
# takes a machine-wide build slot, and a busy machine runs only a handful of
# those against many concurrent checkouts, so the wait is however long the queue
# is — a slot can legitimately hold a build for well over an hour. The compile
# itself is ~3s.
#
# The old fixed 180s was therefore a wall-clock bet on machine load, and it lost:
# measured 2026-09-02, `TimeoutExpired ... after 180 seconds` ERRORed a windows
# test at SETUP on a saturated box, having never launched the app. A green run
# pays only the real build time either way — this ceiling is spent only by a run
# that was going to be slow regardless, and a genuinely wedged build still fails
# bounded rather than hanging the suite.
_BRIDGE_BUILD_BUDGET_S = 1800.0


def _ensure_bridge_built() -> None:
    """Build the FlaUI bridge via its build-if-stale-gated just recipe.

    The bridge runs with ``dotnet run --no-build`` (fast startup); ``just
    windows-flaui-bridge`` rebuilds it only when a ``.cs``/``.csproj`` source
    is newer than the exe. Delegating to the shared just/build-if-stale gate
    keeps freshness logic out of the driver, per
    docs/goal/architecture/build-system.md § Test fixtures ("fixtures trust
    just to handle freshness — they don't do their own mtime comparisons").
    Mirrors conftest's `just web-test` / `just windows-debug` invocations.
    """
    result = subprocess.run(
        ["just", "windows-flaui-bridge"],
        cwd=str(_REPO_ROOT),
        capture_output=True, text=True, timeout=_BRIDGE_BUILD_BUDGET_S,
    )
    if result.returncode != 0:
        raise RuntimeError(
            "FlaUI bridge build (`just windows-flaui-bridge`) failed "
            f"(exit {result.returncode}):\n{result.stderr[-1000:]}\n{result.stdout[-500:]}"
        )
    if not _BRIDGE_EXE.exists():
        raise RuntimeError(
            f"`just windows-flaui-bridge` succeeded but exe not found at {_BRIDGE_EXE}."
        )


class WindowsBridgeDriver(HttpBridgeDriver):
    """Windows E2E driver backed by FlaUI automation bridge.

    Launches the flaui-bridge C# process, then communicates via HTTP.
    No Appium or WinAppDriver needed.
    """

    # The flaui-bridge implements POST /element/scroll-into-view (Actions.cs
    # ScrollIntoView), so wait_for can target offscreen elements directly
    # instead of overshooting them with blind page scrolls.
    _supports_scroll_into_view = True

    # The flaui-bridge serves GET /element/texts and /element/attrs (Actions.cs
    # GetTexts/GetAttrs), so a helper reading a whole column of same-id rows
    # pays one round trip and one tree walk instead of N of each. This is the
    # platform the seam was measured on: every UIA find is served by the app's
    # UI thread, so on a long list the per-element loop dominates a test's
    # entire wall time.
    _supports_bulk_reads = True

    #: How long `recover()` waits for the relaunched app to publish state (i.e.
    #: its TestAgent poll loop is live). Named + generous per convention 14: a
    #: green relaunch pays only the real boot time, and the ceiling sits far
    #: above any non-pathological cold WinUI start under machine load.
    #: How long a fresh or relaunched app gets to publish its first state
    #: before ``launch()``/``recover()`` calls it failed. Generous on purpose
    #: (convention 14): the poll breaks the instant state arrives, so a green
    #: run pays only the real cold-WinUI boot time and only a genuinely dead
    #: launch spends the budget. Raised from 30 s when the poll's verdict
    #: became load-bearing — an advisory budget may be tight, an authoritative
    #: one may not; row 80 confirmed the same class of cost on a FIRST launch
    #: (a fresh process is a low-priority background one until the OS gives it
    #: its first scheduled slice, and a busy shared dev machine's ambient
    #: load — routinely several concurrent build/test jobs, sometimes a
    #: second windows e2e suite — makes that slice arrive anywhere from under
    #: a second to double digits of seconds late), so both callers share this
    #: one budget rather than each guessing their own.
    _RELAUNCH_READY_BUDGET_S = 90.0

    def log_scope_across_relaunch(self) -> str:
        """``"cumulative"`` — the data-dir daily-rolling log is append-shared across relaunches - see `_app_log_since`, which exists to slice from a mark for this reason.

        See the base declaration for what each answer means and why it is
        declared rather than inferred; pinned per driver by
        `tests/test_module_relaunch.py`.
        """
        return "cumulative"

    def is_windows(self) -> bool:
        return True

    def is_nav_tab_revealed(self, element_id: str, *, scope: str | None = None) -> bool:
        """Windows gated nav tabs (``NavAdmin`` / ``NavFamily`` in ``MainPage.xaml``)
        are ``Visibility="Collapsed"`` until their gate fires, so they are ABSENT
        from the UIA tree until revealed and PRESENT once revealed — ``count() >= 1``
        is therefore an exact reveal signal (a revealed tab that lands below the fold
        still counts 1: in-tree, only its rect is offscreen).

        Crucially this is HANG-FREE, unlike the base ``is_visible_scrolled``: it never
        issues a UIA ``ScrollIntoView`` / ``SetScrollPercent`` sweep over the
        ``NavigationView`` pane. That sweep intermittently HANGS the FlaUI bridge (the
        standing "ScrollIntoView deep into a scroll container can hang the bridge"
        gotcha) — it took the bridge down mid-``test_family.py`` (the
        ``is_visible_scrolled`` reveal check, root-caused: the app is healthy and
        reveals the tab; the bridge dies on the scroll of the below-fold footer row).
        A gated nav tab's reveal is tree-membership, never pixel-visibility, so no
        scroll is warranted."""
        return self.count(element_id, scope=scope) >= 1

    def is_absent(self, element_id: str, *, scope: str | None = None) -> bool:
        """windows' ``/element/visible`` is ``elements.Length > 0 &&
        !elements[0].IsOffscreen`` (``Actions.IsVisible``) — the SAME
        ``WalkScope`` + ``FindAll`` lookup ``Actions.Count`` does, plus a viewport
        predicate. So ``not is_visible(x)`` reads "absent OR one scroll away", and
        an element a defect really did paint below the fold answers *not visible*
        for the boring reason.

        ``count == 0`` is the same question minus the viewport confound: exact in
        both directions (an offscreen-but-painted element still counts), and it
        issues NO scroll — a UIA ``ScrollIntoView`` deep in a scroll container can
        take the FlaUI bridge down outright (see ``is_nav_tab_revealed``).

        ⚠ Exact only where "absent" means "out of the UIA tree" — a
        ``Visibility``-gated element, an empty ``ItemsSource``, a page that is not
        the current one — and only where the container does NOT virtualize: an
        unrealized row counts 0 for the boring reason. Check the surface's
        ``ItemsPanel`` before adopting it there (``FeedPage.xaml``'s lists override
        it with a plain ``StackPanel`` and so are safe)."""
        return self.count(element_id, scope=scope) == 0

    def type_physical(self, element_id: str, text: str, *, index: int = 0,
                      pre_delay_ms: int = 0) -> dict:
        """DIAGNOSTIC: type ``text`` through the bridge's PHYSICAL ``SendInput`` path,
        optionally holding the machine-wide input critical section open for
        ``pre_delay_ms`` first (``Actions.TypePhysical``).

        Not a substitute for :meth:`type_text` — ordinary tests must keep using the
        ValuePattern path, which is focus-free and immune to foreground churn. This
        exists so ``test_flaui_input_lock_windows.py`` can construct a DETERMINISTIC
        overlap between two bridges' input sections, which is the only way to show
        ``Actions._inputMutex`` actually protects the victim app rather than merely
        not regressing anything.

        Returns the bridge's foreground report — ``app_hwnd`` plus the foreground
        window observed right after ``ForegroundApp()`` and again at the instant of
        injection. Those two are what separate "we never won the foreground" from
        "we won it and lost it", which the typed text alone cannot tell apart.
        """
        return self._post("/element/type_physical", {
            "id": element_id,
            "text": text,
            "index": index,
            "pre_delay_ms": pre_delay_ms,
        }, timeout=pre_delay_ms / 1000.0 + 60)

    def debug_tree(self, *, anchor: str | None = None, depth: int = 6,
                   raw: bool = False) -> dict:
        """Dump a UIA subtree (diagnostic only — never assert on this).

        ``anchor`` roots the dump at an AutomationId/ClassName (e.g.
        ``"NavigationView"``); ``raw=True`` walks the UIA *raw* view rather than
        the search view a test sees. Comparing the two answers the question no
        property guess can: is an element **absent** from the tree, or **present
        but pruned/zero-sized**? See ``Actions.DumpTree``.
        """
        params: dict = {"depth": str(depth)}
        if anchor:
            params["anchor"] = anchor
        if raw:
            params["raw"] = "1"
        return self._get("/debug/tree", params)

    def download_dir(self) -> str | None:
        """Where FaunaApp saves e2e-mode downloads (no save dialog under e2e).

        Mirrors the app-side seam (`SnapshotFileSaver`): with `FAUNA_E2E_BRIDGE`
        set the app writes to `FAUNA_E2E_DOWNLOAD_DIR` when the launch env
        carried one, else `<BackupPaths.DataDir>\\e2e-downloads` — i.e. it follows
        the (now isolated by default) data dir, NOT the real `%LocalAppData%`.
        The bridge launches the app on this same machine, so the path is directly
        readable by the test process.
        """
        import os

        env = (self._session_body or {}).get("environment", {})
        override = env.get("FAUNA_E2E_DOWNLOAD_DIR")
        if override:
            return override
        data_dir = env.get("FAUNA_E2E_DATA_DIR") or self._data_dir
        if data_dir:
            return os.path.join(data_dir, "e2e-downloads")
        local = os.environ.get("LOCALAPPDATA")
        if not local:
            return None
        return os.path.join(local, "Fauna", "e2e-downloads")

    def data_dir(self) -> str | None:
        """This launch's isolated data dir (`FAUNA_E2E_DATA_DIR`) — `BackupPaths.DataDir`
        on the app side, the base every actor-scoped store (the MLS db, drafts,
        the audit-loop state file) resolves under — `segment-backup` was one
        such store too until the in-app coordinator was deleted fleet-wide
        2026-08-15 (`backup-restore.md` § Background Tasks → *Flip status
        (slice 5)*). Falls back to the real `%LocalAppData%\\Fauna` only when
        this launch never isolated one (mirrors `download_dir`'s fallback).
        """
        import os

        env = (self._session_body or {}).get("environment", {})
        data_dir = env.get("FAUNA_E2E_DATA_DIR") or self._data_dir
        if data_dir:
            return data_dir
        local = os.environ.get("LOCALAPPDATA")
        return os.path.join(local, "Fauna") if local else None

    def app_running(self) -> bool:
        """Whether the launched app process is still alive.

        The windows app child belongs to the FlaUI **bridge**, not to this python
        process, so unlike the linux/tui drivers there is no `Popen` to `poll()`.
        The bridge answers for it over `GET /session/status`.

        Without this the shared `expect_launch_refused` helper found no process
        handle on this driver and returned **vacuously green** — it would have
        passed against an app that launched perfectly, which is precisely the
        "green test that lies by never running" trap. Any failure to reach the
        bridge reads as "not running", so a dead bridge never masquerades as a
        surviving instance.
        """
        try:
            return bool(self._get("/session/status").get("running"))
        except Exception:
            return False

    def get_attr(
        self, element_id: str, attribute: str, index: int = 0, *, scope: str | None = None
    ) -> str | None:
        """The bridge's attribute read, but for the compose field's ``text-runs``.

        The compose field's styling lives in the app's live RichEdit document, which
        UIA does not carry (the field's HelpText is already its ``visible`` channel,
        and every other attribute falls back to it). So ``text-runs`` on
        ``dm-text-field`` is the app's ``compose_text_runs`` command, which reads the
        active compose field's runs in-process in linux's JSON shape
        (``MarkdownRichEditBox.TextRuns``) — the windows twin of linux's agent read and
        apple's ``NSTextStorage`` read. Returned as the JSON string the other drivers
        return."""
        if attribute == "text-runs" and element_id == "dm-text-field":
            runs = self.call_command("compose_text_runs")
            return None if runs is None else json.dumps(runs)
        return super().get_attr(element_id, attribute, index, scope=scope)

    # WinUI's InfoBar contributes its severity glyph's accessible name ("Error
    # icon", "Warning icon", …) ahead of the message in the element's aggregated
    # text. That is chrome, not the message: no other app's `error-message` carries
    # it, so a test comparing the shown text to the catalogued string could never
    # hold on windows. One strip here replaces the per-helper copies (each actions
    # class re-deriving it, or a test settling for `endswith`) — a red test proved
    # the gap in a sixth place, `test_device_removal_refusal.py`.
    _INFOBAR_SEVERITY_ICON = re.compile(r"^(?:Error|Warning|Informational|Success) icon\s+")

    def get_text(self, element_id: str, index: int = 0, *, scope: str | None = None) -> str:
        text = super().get_text(element_id, index, scope=scope)
        return self._INFOBAR_SEVERITY_ICON.sub("", text) if isinstance(text, str) else text

    def in_viewport(self, element_id: str, *, index: int = 0,
                    scope: str | None = None) -> bool:
        """The bridge's ``in-viewport`` attribute (`flaui-bridge/Actions.cs`):
        true when the element's vertical centre lies inside its nearest
        vertically-scrollable ancestor's visible band — linux's and tui's rule,
        in UIA geometry. WinUI realizes rows the viewport does not show, so, as
        on those two, the element registry alone cannot say "on screen"; the
        bounding rectangles do.

        Read-only: it never scrolls, unlike `scroll_to_visible_fraction` below.
        """
        value = self.get_attr(element_id, "in-viewport", index, scope=scope)
        if value is None:
            raise AssertionError(
                f"windows answered no in-viewport for {element_id!r}[{index}] — the "
                "element is missing or its geometry could not be read; "
                f"{self.diagnose(element_id, scope=scope)}"
            )
        return value == "true"

    def scroll_to_visible_fraction(
        self, element_id: str, min_fraction: float, *, index: int = 0,
        scope: str | None = None
    ) -> float:
        """Targeted scroll to a MEASURED visibility fraction — the precise
        sibling of the base `scroll_to()` / `wait_for`'s targeted scroll,
        whose "found" is UIA's own `IsOffscreen`, which flips as soon as any
        sliver of the element is on screen (empirically as low as ~25% in a
        typical feed post-card layout — `Actions.ScrollIntoView`'s own doc
        comment). Use this when a test needs a specific mid-list visibility
        (e.g. proving a real "substantially visible" dwell exposure), not
        merely "on screen at all".

        Returns the achieved fraction (0.0-1.0) once the bridge's scroll
        sweep ends, whether or not `min_fraction` was reached — the caller
        decides what to do with a fraction that fell short. Raises
        `LookupError` when the element itself was never found (mirrors
        `scroll_to`'s fail-loud contract: a dwell test must never silently
        measure nothing).
        """
        body: dict = {"id": element_id, "min_fraction": min_fraction, "index": index}
        wire = self._scope_wire(scope)
        if wire:
            body["scope"] = wire
        resp = self._post("/element/scroll-into-view-fraction", body)
        if not resp.get("found", False):
            raise LookupError(
                f"scroll_to_visible_fraction({element_id!r}[{index}]): not found"
            )
        return float(resp.get("fraction", 0.0))

    # ------------------------------------------------------------------
    # Window-level ops (graceful quit — mirrors linux driver's window_close/
    # is_app_alive/wait_app_exit; TrayIconService.QuitApplication()'s real
    # quit path, Actions.WindowClose owns the WM_CLOSE mechanism)
    # ------------------------------------------------------------------
    def window_close(self) -> None:
        """Simulate the titlebar close button — posts a real WM_CLOSE to the
        app's main window HWND via the FlaUI bridge, running the real
        `AppWindow.Closing` handler so close-to-tray's hide-vs-quit decision,
        and (when it decides to quit) `TrayIconService.QuitApplication()`'s
        real flush-then-exit sequence, both run end-to-end. Tolerates a
        dropped connection: a quit-path close can exit the app process (and
        race the HTTP reply) before this call would otherwise return. Assert
        the outcome via `is_app_alive()` / `wait_app_exit()`.
        """
        try:
            self._post("/window/close", {})
        except Exception:
            pass

    def is_app_alive(self) -> bool:
        """True while the launched FaunaApp.exe process is still running.

        Same source as `app_running()` — the bridge's tracked process handle,
        via `GET /session/status` (`SessionManager.AppStatus`). Named to
        mirror the linux/tui drivers' `is_app_alive()`."""
        return self.app_running()

    def app_package_full_name(self) -> str | None:
        """The package identity the launched FaunaApp.exe runs with, or ``None``
        when it has none — the OS's answer (`GetPackageFullName` on the bridge's
        tracked process), for a test whose premise is a packaged launch
        (`launch(config)`'s ``package_family_name``)."""
        try:
            return self._get("/session/status").get("package_full_name")
        except Exception:
            return None

    def wait_app_exit(self, timeout: float = 10.0) -> bool:
        """Block until the launched app process exits; True if it exited
        within `timeout`.

        Unlike linux/tui this driver holds no local process handle — the
        FlaUI bridge does, as the app's actual parent — so this deadline-polls
        `/session/status` instead of a Popen `wait()`."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self.is_app_alive():
                return True
            time.sleep(0.2)  # sleep-ok: poll cadence inside a deadline poll
        return not self.is_app_alive()

    def app_log_text(self) -> str | None:
        """This launch's on-disk app log, for corroborating WHY a process died.

        windows' equivalent of the linux/tui drivers' captured stderr — the same
        role macOS's unified-log read plays. It is deliberately not a redirected
        stderr pipe: an inherited-but-undrained pipe is the wedge e2e convention
        13 exists to forbid.

        Reads the launch's OWN data dir (`FAUNA_E2E_DATA_DIR`), never
        `%LocalAppData%` — since 2026-07-22 every windows launch is isolated, so
        the real profile's log belongs to the *installed* product, not to us.
        """
        import glob
        import os

        env = (self._session_body or {}).get("environment", {})
        data_dir = env.get("FAUNA_E2E_DATA_DIR") or self._data_dir
        if not data_dir:
            return None
        chunks = []
        for path in sorted(glob.glob(os.path.join(data_dir, "logs", "*"))):
            try:
                with open(path, "r", encoding="utf-8", errors="replace") as fh:
                    chunks.append(fh.read())
            except OSError:
                continue
        return "\n".join(chunks) if chunks else None

    def app_stderr_text(self) -> str:
        """This launch's captured app+agent log so far ("" if none yet).

        windows' analogue of the linux/tui/macos drivers' `app_stderr_text()`
        (same contract — always a `str`, never `None` — so a client-parametrized
        test reads all of them uniformly): `helpers/folder_content.py`'s
        `agent_diagnosis`/`await_agent_upload` call this unconditionally, and
        without it here they silently caught the resulting `AttributeError` and
        diagnosed against `""` forever.

        Concatenates `app_log_text()` with the isolated sync agent's OWN
        daily-rolling log under `FAUNA_E2E_SYNC_AGENT_DATA_DIR`
        (`conftest.py::_apply_isolated_sync_agent_env`, the `isolated_sync_agent`
        marker) when that env var was set for this launch. Needed because,
        unlike linux (which direct-spawns the real agent as an inherited-fd
        child, so its output lands in the SAME stderr the app driver already
        captures), windows spawns the agent DETACHED — its output never
        reaches the app's own log. `bins/fauna-sync-agent`'s `run_main` writes
        its own `<data-dir>/logs/fauna.log.<date>` there (`fauna_log::init`),
        the exact glob shape `app_log_text()` already reads for the app.
        """
        return stitch_agent_log(
            self.app_log_text() or "",
            detached_agent_log_text(self.sync_agent_state_base),
        )

    def __init__(self):
        super().__init__()
        self._bridge_proc: subprocess.Popen | None = None
        self._app_path: str = ""
        self._session_body: dict | None = None
        self._cred_dir: str | None = None
        self._owns_cred_dir: bool = False
        self._keyring_app: str | None = None
        self._cred_store_owner_config: dict = {}
        self._data_dir: str | None = None
        self._owns_data_dir: bool = False
        self._local_appdata: str | None = None
        self._owns_local_appdata: bool = False
        self._launched_once: bool = False

    @property
    def sync_agent_state_base(self) -> str | None:
        """Where THIS launch's `fauna-sync-agent` keeps its state — the agent's
        `--data-dir`, the BASE the per-actor scope nests under:
        `service.rs::apply_actor_scope` scopes every layout since 2026-09-26, the
        override included (`on-demand-files.md` § Multi-account × File Provider,
        consequence 3). Until then the override stayed flat, so every account one
        launch signed in shared one binding store and one `fsid-<ref>.db` per
        folder id — two fresh nests both mint `local:N`, so a later account
        inherited the earlier one's pull anchor. `helpers/sync_agent_config.py`
        globs flat first, then per-actor, so a reader never needs the actor id.

        windows has no `config_home` — that is an XDG concept, and this launch
        relocates `%LOCALAPPDATA%` instead — so the answer has to come from the
        driver, exactly as tui's does on this platform (`drivers/tui.py`).

        The answer is the `--data-dir` the launch pinned in
        `FAUNA_E2E_SYNC_AGENT_DATA_DIR` — a SESSION-scoped dir, since the app
        itself is session-scoped (`conftest.py::isolated_sync_agent_data_dir`);
        a fresh folder-share seat gets a per-seat dir and pipe instead
        (`conftest.py::_own_windows_share_seat_agent`), read back here the same way.
        One agent per launch serves the launch's pipe on that dir, whichever
        process started it: a fixture that needs the agent up before login
        adopts or starts it there (`helpers/windows_sync_agent.serving_agent`),
        never a second agent on a dir of its own — that one exits as a duplicate
        while the pipe goes on answering, and a driver answering its dir would
        point every reader at a directory nothing was written to.

        NEVER `_resolved_store_root` (`<LOCALAPPDATA>\\Fauna\\sync`): that is the
        unified ACCOUNT store's root, a declared sibling of the agent's flat base
        (see `launch()`), and the custodian store is not under it — answering it
        would search an empty tree and read as "the pass stored nothing".

        `None` before the first launch, or on a launch that pinned no agent dir
        (the agent, if any, is the box's own — not ours to read), which
        `agent_state_base` names loudly instead of guessing.
        """
        env = (self._session_body or {}).get("environment", {})
        return env.get("FAUNA_E2E_SYNC_AGENT_DATA_DIR")

    @property
    def sync_agent_pipe(self) -> str | None:
        """The full Win32 pipe path THIS launch's agent serves
        (``\\\\.\\pipe\\<FAUNA_E2E_SYNC_PIPE>``), for a test that must talk to it
        directly — `None` on a launch that pinned no pipe (the agent, if any, is
        the box's own, not ours to address). Same property on tui's driver."""
        leaf = (self._session_body or {}).get("environment", {}).get("FAUNA_E2E_SYNC_PIPE")
        return rf"\\.\pipe\{leaf}" if leaf else None

    @property
    def bridge_url(self) -> str:
        """This driver's bridge URL — what an app process sets ``FAUNA_E2E_BRIDGE``
        to in order to report here.

        Public because a **spawned** instance needs an automation channel of its own
        (``account-open-new-instance-button``). Windows' test agent is an outbound
        *poller* against a bridge URL, not a server on a port, so it has no analogue
        of linux's/apple's "hand the child a free ``FAUNA_E2E_AGENT_PORT``"; a child
        that inherited its parent's bridge would steal the parent's commands and
        clobber its state pushes (the bridge serves one agent per session epoch).
        So the app never passes its own bridge down — a test that wants to observe a
        spawned child stands up a second, app-less bridge with
        :meth:`start_bridge_only` and names it in ``FAUNA_E2E_CHILD_BRIDGE``."""
        return self._url

    def top_level_windows(self) -> list[dict]:
        """The OS's own list of this app's top-level windows — one
        ``{"title": str, "is_offscreen": bool}`` per window, empty when the app
        owns none.

        The corroborating half of the ``--autostart`` tray-residency assertion
        (`apps/windows.md` § App Lifecycle → *Auto-start at sign-in*): the app
        publishes its own activation decision into ``state["launch"]``, which
        proves the code took the branch it meant to, and this proves the branch
        had the effect it claims. Read AFTER the published decision, never
        polled-with-a-sleep — the decision key's presence is the causal barrier
        (e2e convention 14).

        Windows-only, deliberately: the OS handle enumeration has no cross-app
        analogue, so a test that needs it branches on ``hasattr`` rather than
        pretending every driver can answer."""
        return self._get("/session/windows").get("windows", [])

    def foreground_report(self, *, clear: bool = False) -> dict:
        """Who owns the desktop's foreground, and which bridge gestures took it.

        ``{"foreground_hwnd", "foreground_pid", "app_pid", "app_owns_foreground",
        "takes"}`` — ``takes`` lists every gesture the bridge recorded as moving the
        foreground onto the app since the last ``clear=True`` read (physical
        ``SendInput`` and UIA ``SetFocus``, which activates the window). The UIA
        gesture paths never appear there. e2e convention 10's windows focus axis:
        a harness-launched app never takes the keyboard focus from the person at
        this desktop except through one of those named fallbacks.

        A disconnected session reports ``foreground_hwnd`` 0: there is no
        foreground to take, so ``app_owns_foreground`` proves nothing there — the
        ``takes`` record and the app's published activation decision do.

        Windows-only, like :meth:`top_level_windows`."""
        return self._get("/session/foreground", {"clear": "1"} if clear else None)

    def live_app_instance_count(self) -> int | None:
        """How many live `FaunaApp.exe` processes own this driver's
        `FAUNA_E2E_DATA_DIR` (base-class contract). Answered by the OS via the
        bridge's `GET /session/app-instances` — the process environment the kernel
        recorded at creation, so an instance stranded in the account chooser (which
        never starts its agent and so can never report itself) is counted exactly
        like a healthy one. A bridge that cannot answer returns None rather than a
        wrong number: a broken count must not read as a leak."""
        try:
            return len(self._get("/session/app-instances").get("instances", []))
        except Exception:  # noqa: BLE001 - an unanswerable probe is not a finding
            return None

    def start_bridge_only(self) -> str:
        """Start this driver's bridge WITHOUT launching an app, and return its URL.

        The observer half of the spawn-button test: the bridge's ``/app/state`` is
        populated by whichever agent reports to it, so a driver that never posted
        ``/session`` can still read a spawned child's state (``get_state`` /
        ``wait_for_state``). UI queries are not available on such a driver — there is
        no FlaUI session behind them — which is exactly the contract, since the child
        is driven by nothing and only observed."""
        self._start_bridge()
        return self._url

    def _start_bridge(self) -> None:
        """Bring up the FlaUI bridge process and block until it is healthy."""
        # Build the bridge if the exe is missing or any source is newer
        # (dotnet run uses --no-build, so a stale exe would run silently).
        _ensure_bridge_built()

        # Start the bridge process.
        #
        # This is the ROOT of the windows app tree: bridge -> FaunaApp.exe ->
        # (since the C# sync stack retired onto the shared provisioner)
        # fauna-sync-agent.exe, a process deliberately built to outlive its
        # parent. So point 9's die-with-the-run guarantee has to be armed here or
        # a killed run leaks all three — and on windows `popen_group_kwargs()` is
        # `{}` by construction, which makes the SECOND call below the whole
        # mechanism (e2e-conventions.md § point 9).
        self._bridge_proc = subprocess.Popen(
            ["dotnet", "run", "--project", str(_BRIDGE_DIR), "--no-build"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            # errors="replace": a single undecodable byte on this pipe (the
            # bridge's inherited-handle app under test writes to the SAME
            # stderr — see the DiagLogPath doc comment in Program.cs) used to
            # raise inside the drain thread, which port_util.drain_pipes
            # swallows silently — killing the reader with no error surfaced.
            # Once dead, nothing drains this pipe, so the NEXT bridge-side
            # stderr write blocks once the OS buffer fills, holding whatever
            # lock or gate that write happened to be inside -- the `_uiaGate`
            # release-order bug this was found alongside. Never crash the reader over one bad byte.
            errors="replace",
            **popen_group_kwargs(),
        )
        reap_descendants_of(self._bridge_proc.pid)

        # Register for atexit cleanup
        _live_bridges.append(weakref.ref(self))

        # Read the port from stdout
        port = None
        deadline = time.monotonic() + 60  # first run includes dotnet build
        while time.monotonic() < deadline:
            if self._bridge_proc.poll() is not None:
                stderr = self._bridge_proc.stderr.read()
                raise RuntimeError(f"Bridge exited early. stderr: {stderr[:500]}")
            line = self._bridge_proc.stdout.readline()
            if line.startswith("BRIDGE_PORT="):
                port = int(line.strip().split("=")[1])
                break
        if port is None:
            self._bridge_proc.kill()
            stderr = self._bridge_proc.stderr.read()
            raise RuntimeError(f"Bridge didn't report port. stderr: {stderr[:500]}")

        # ⚠ DRAIN THE BRIDGE PIPES FOR THE REST OF THE RUN — load-bearing, not
        # hygiene. The handshake above stops reading at "BRIDGE_PORT=", and the app
        # under test INHERITS these very handles (SessionManager.Launch starts it
        # with UseShellExecute=false and no redirect of its own), so an undrained
        # buffer here blocks the app's UI thread inside a native log write. Full
        # mechanism + why it read as a product bug for seven sessions:
        # port_util.drain_pipes.
        #
        # The `[bridge] …` lines are ALSO tee'd live to stderr. The deque alone is
        # not enough: it is only ever read by a formatted failure report
        # (`_bridge_lines`), and the pytest-timeout watchdog kills the process
        # without formatting one — so a hang long enough to trip the watchdog is
        # exactly the hang whose explanation gets discarded. A tee'd line is
        # already in captured stderr, which the timeout dump does print. The
        # filter keeps the app's own inherited native log volume off the critical
        # path; only the bridge's own narration goes live.
        # `[fauna] FATAL` rides along: the app installs three unhandled-exception
        # hooks that report to THIS stream (App.xaml.cs), so a crash already says
        # what it was — and every reader of this buffer filtered the report out,
        # because both display paths matched on `[bridge]` alone. Measured: the app
        # died 0xC000027B in Microsoft.UI.Xaml.dll and its own report was captured
        # and then discarded, leaving a bridge timeout as the only visible symptom.
        # Only the header line is tee'd; the trace that follows comes back in full
        # from `_bridge_lines`, which has the whole buffer to scan.
        self._bridge_log = drain_pipes(
            self._bridge_proc,
            echo=sys.stderr,
            echo_filter=lambda line: "[bridge]" in line or "[fauna] FATAL" in line,
        )

        self._url = f"http://127.0.0.1:{port}"

        # Wait for health
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                resp = self._get("/health")
                if resp.get("ready"):
                    break
            except Exception:
                time.sleep(0.3)

    def launch(self, config: dict) -> None:
        # Stored purely for a caller that needs to re-launch from a MODIFIED copy
        # of its own original config (e.g. dropping `seed_credentials` for a
        # no-reseed relaunch, `test_add_account_provisioning.py`) — every other
        # driver keeps this (`linux.py`/`tui.py`/`macos.py`/`ios.py`), and nothing
        # here reads it back: `launch_environment()` below still reads
        # `_session_body`, since that reflects what `recover()` actually re-posts
        # on a relaunch, unlike this pristine, never-updated copy of the FIRST
        # launch's input.
        self._launch_config = config
        self._start_bridge()

        # Launch the app
        self._app_path = config["app_path"]
        app_args = config.get("args", "")
        # The nest URL key is `url` across every _build_app_config branch
        # (uniform with the web/macos/ios drivers, which read config["url"]).
        # Reading "nest_url" here was drift: the app launched with NO
        # --nest-url and fell back to a stale persisted nest (App.xaml.cs:308),
        # leaving it Offline against a dead port — which is why the first test
        # that needs a live nest fetch (test_all_media_cross_set) rendered 0.
        #
        # Only on this driver instance's FIRST launch, though: `--nest-url` is a
        # launch-machine persistence override that wins over the vault
        # (apps/windows.md § CLI arguments), so re-appending it on every relaunch
        # made a windows relaunch unable to take the "(identity, no node_url)"
        # startup branch every other driver's relaunch can reach. `recover()` is unaffected: it re-posts the frozen
        # `_session_body["args"]` directly and never calls `launch()` again.
        if config.get("url") and not self._launched_once:
            app_args += f" --nest-url {config['url']}"
        if "secret_hex" in config:
            app_args += f" --secret {config['secret_hex']}"
        # e2e convention 10's windows focus axis: a harness-launched window is
        # SHOWN, never ACTIVATED, so a run never takes the keyboard focus from the
        # person working in this Windows session (an RDP user shares it with every
        # launch). The app honours the flag only in a test-agent build (`DEBUG ||
        # FAUNA_E2E_AGENT`, App.xaml.cs), so it is artifact-set wiring, never a
        # user knob. It survives `recover()`, which re-posts these args minus
        # `--nest-url` only. `activate_window=True` is the explicit opt-out for a
        # test whose subject is the activation itself.
        if not config.get("activate_window") and NO_ACTIVATE_ARG not in app_args.split():
            app_args += f" {NO_ACTIVATE_ARG}"

        env = config.get("environment", {})
        env["FAUNA_E2E_BRIDGE"] = self._url
        # The resident engine's full-reconcile cadence (`always_resident::rescan_interval`,
        # the compile-gated `FAUNA_E2E_RESCAN_MS` seam) — linux's/tui's/macOS's launches
        # already default it (`drivers/linux.py`, `drivers/tui.py`, `drivers/macos.py`);
        # this one never did, so a windows launch ran the 300s production cadence with no
        # test able to see or budget for it — the exact gap that reddened macOS's own first
        # mass-delete-floor run (`test_filesync_mass_delete_floor.py`'s 260s hold budget vs.
        # the un-overridden 300s cadence). Inherited by the spawned `fauna-sync-agent.exe`
        # child. A caller-supplied override in `config["environment"]` (merged above) wins.
        env.setdefault("FAUNA_E2E_RESCAN_MS", "30000")
        _default_sync_agent_pin(env)

        # The app's own agent trace (`FaunaApp.Core/Logs/E2eTrace.cs`), reachable
        # from a test that does NOT own its driver. Tests that build their own
        # driver already set this in `config["environment"]` (the worked example is
        # `test_atproto_custody_alarm.py`), but every test riding the cached `app` /
        # `logged_in_app` fixture had no way to turn it on at all — so the one log
        # that answers "what did the poll loop actually do" was unreachable for the
        # majority of the suite. That gap is not hypothetical: three separate
        # `App did not acknowledge command … within Ns` investigations
        # each
        # stalled on it, and the earliest investigation's own notes name
        # plumbing this as step one.
        #
        # ⚠ This is a deliberate exception to e2e convention 10 (a launch is
        # isolated from the box it runs on), and a narrow one: the variable names a
        # WRITE-ONLY diagnostic sink, feeds no app behavior, and is off unless a
        # human exported it for this run. A caller-supplied value still wins, so a
        # test owning its driver keeps its own per-test path. `E2eTrace` takes the
        # path verbatim under a lock and is compiled out of release builds
        # (convention 15), so nothing here reaches a shipped artifact.
        if "FAUNA_E2E_AGENT_LOG" not in env:
            inherited = os.environ.get("FAUNA_E2E_AGENT_LOG")
            if inherited:
                env["FAUNA_E2E_AGENT_LOG"] = inherited

        # The SHARED-RUST half of the same diagnostic gap. `E2eTrace` above
        # carries what the C# shell did; everything the Rust core decides goes
        # through `tracing`, which `FaunaFfiMethods.InstallLogging` files under
        # `<data_dir>/logs/fauna.log.<date>` -- filtered by `RUST_LOG`, whose
        # default is `info` (`libs/fauna-log/src/lib.rs`). So every `debug!` a
        # shared crate writes was unreachable from a windows run, and no
        # per-feeder / per-stage line could ever be read here. linux and tui
        # already inherit it from the pytest process (their launches pass
        # `os.environ` straight through); this one built its env dict from
        # scratch and dropped it, which is why the stalling sweep pass
        # could be bracketed but never localised.
        #
        # Same convention-10 exception as the line above, and narrower: this
        # names a log LEVEL, feeds no app behaviour, and is off unless a human
        # exported it. `RUST_LOG` is already the sanctioned spelling for exactly
        # this ("the nest's own diagnostic level, not a deployment input" --
        # `conftest.py`'s `_DOCKER_EXTRA_ENV` note). A caller-supplied value in
        # `config["environment"]` still wins.
        if "RUST_LOG" not in env:
            inherited_log = os.environ.get("RUST_LOG")
            if inherited_log:
                env["RUST_LOG"] = inherited_log

        # Credential store: point the app at a per-test FILE backend instead of
        # the machine's real Credential Manager (App.xaml.cs → CredentialStore).
        # Two reasons, both mirroring the linux driver:
        #
        #  1. A test can pre-seed a whole multi-account `AccountRegistry` state
        #     (`tests/common/accounts.py::build_registry_seed`) by writing the file
        #     the store reads on its first `get` — no app code, no UI.
        #  2. The suite stops writing identities into the dev machine's real
        #     Credential Manager, so parallel sessions can't collide there.
        #
        # Kept on the driver (not a fresh dir per launch) because `recover()` re-posts
        # this same session body, so the dir it names has to outlive the process that
        # opened it. What the relaunch decides is therefore not WHICH dir the new
        # process gets but whether its predecessor's credentials are still in it:
        # `_erase_credential_store_for_relaunch` empties it by default, so the app
        # comes back signed out like every other driver's, and
        # `preserve_state_across_relaunch()` is the opt-out for a test asserting
        # client-side durability across a restart. `config["credential_dir"]`, when the
        # caller supplies one (`helpers/skew_client.state_home_config`'s version-skew
        # at-rest grid — the seam that hands ONE store to a SEQUENCE of separate driver
        # instances/processes), takes precedence over the per-instance mkdtemp default —
        # mirrors `drivers/linux.py`'s `config.get("credential_dir") or ...` fallback.
        # Isolation otherwise comes from the unique mkdtemp dir, so the namespace only
        # has to agree with the app (CredentialStore.DefaultNamespace).
        if self._cred_dir is None:
            supplied = config.get("credential_dir")
            self._cred_dir = supplied or tempfile.mkdtemp(prefix="fauna-e2e-creds-")
            self._owns_cred_dir = not supplied  # a caller-supplied dir outlives THIS
            os.makedirs(self._cred_dir, exist_ok=True)  # instance — teardown must not delete it
            self._keyring_app = config.get("keyring_app") or "fauna-windows"
            # Whether the principal-slot carry may restore into this store belongs to
            # the launch that CHOSE it: a caller naming the dir or the namespace owns
            # it, and a later launch() reuses it whatever its own config says.
            self._cred_store_owner_config = {
                key: config.get(key) for key in ("credential_dir", "keyring_app")
            }
        env["FAUNA_E2E_CREDENTIAL_DIR"] = self._cred_dir
        env["FAUNA_KEYRING_APP"] = self._keyring_app
        # The store principal survives a relaunch (convention 10's carve-out,
        # `HttpBridgeDriver._begin_principal_slot_carry`), begun BEFORE the
        # `_resolved_*` attrs below. A first launch has nothing to harvest; what it
        # settles is whether the carry is live. windows' relaunch is `recover()`,
        # which restarts the carry there.
        self._begin_principal_slot_carry(self._cred_store_owner_config, env)
        # The install-device-secret's own leg of the same carry (convention 10,
        # `HttpBridgeDriver._begin_install_device_secret_row_carry` — the row
        # twin of the file-shaped `_carry_install_device_secret` linux/tui use).
        # windows keeps the secret as a ROW (`install/device_secret`) in this
        # same file-backed store rather than a file under an install dir, so it
        # extends the row-shaped leg apple already built rather than growing a
        # second carry (`sync-agent-credentials.md` § Implementation status
        # today, the derived named-row id paragraph's windows sentence).
        # Reuses the `_principal_slot_carry_live` decision the call above just
        # made.
        self._begin_install_device_secret_row_carry(
            os.path.join(self._cred_dir, f"{self._keyring_app}.json"))
        # The generic cross-driver contract linux/tui/macos/ios already publish:
        # the store THIS launch resolved, named the same way on every driver so a
        # helper can read it back without knowing which app it is holding
        # (`common.cred_store.attach_cred_store`). windows kept the same two values
        # under private names only — the outlier, not a different mechanism.
        self._resolved_credential_dir = self._cred_dir
        self._resolved_keyring_app = self._keyring_app

        # `%LOCALAPPDATA%` itself — the ANCESTOR every windows path derivation hangs
        # off, and the third windows isolation axis (e2e-conventions.md point 10).
        # `FAUNA_E2E_DATA_DIR` below closes the app's own flat base, but shared Rust
        # never reads that variable: `fauna_account_store::root::production_base()`
        # reads `LOCALAPPDATA` directly and returns `%LOCALAPPDATA%\Fauna\sync` — the
        # unified account-store root (`account-data-plane.md` § The account store),
        # a DIFFERENT root than the flat base, which is exactly why
        # `account-scoping.md` § Erasure follows scope calls it "a sibling of an app's
        # flat base, not a child of it" and why the erase pair takes it separately.
        #
        # Un-relocated, that root resolved the real developer profile on every windows
        # launch, and the leak was not read-only: `AccountStateDir.EraseAll()` (the UI
        # sign-out, `App.xaml.cs::ClearCredentialNamespace`) calls
        # `AccountStateEraseAllScopes(..., storeContainer: null)`, which
        # `libs/fauna-ffi/src/account_state.rs::store_root_for` maps to
        # `StoreRoot::platform()`, and `fauna_account_store::db` then
        # `remove_dir_all`s EVERY well-formed 64-hex actor dir it finds there. So one
        # `test_sign_out.py[windows]` swept whatever real accounts the developer's
        # `%LOCALAPPDATA%\Fauna\sync` held — a destructive convention-10 breach, not a
        # stale read.
        #
        # Relocating the ancestor rather than patching each derived path is what
        # linux/tui (`XDG_CONFIG_HOME`) and macOS (`HOME`) already do — one variable,
        # both roots, no per-path table to keep in sync — and it closes two more
        # derivations for free: `cert_binding::windows_trust_home` (the install-scoped
        # pin store a `--data-dir`-less agent consumes) and every other
        # `%LOCALAPPDATA%`-derived agent path.
        if self._local_appdata is None:
            supplied_local = config.get("local_appdata")
            self._local_appdata = supplied_local or tempfile.mkdtemp(
                prefix="fauna-e2e-localappdata-"
            )
            self._owns_local_appdata = not supplied_local
            os.makedirs(self._local_appdata, exist_ok=True)
        env["LOCALAPPDATA"] = self._local_appdata
        #: The unified store root THIS launch resolved — `production_base()`'s
        #: derivation spelled once on the harness side so a helper
        #: (`tests/common/scope_store.py`) can read it back without re-deriving it.
        self._resolved_local_appdata = self._local_appdata
        self._resolved_store_root = os.path.join(self._local_appdata, "Fauna", "sync")

        # Data dir: point the app's `%LocalAppData%\Fauna` equivalent (the MLS store,
        # trace log, drafts, config-replica, nest-identity pin store — `BackupPaths.DataDir`
        # in the app) at a per-driver-instance root instead of the dev machine's real
        # profile. Defaulted, exactly like the credential store above, because a launch
        # must be isolated from the box it runs on (e2e rule 10, `testing.md` § conventions
        # point 10): the real `%LocalAppData%\Fauna` is ALSO the profile of the *installed*
        # production app, which auto-starts at sign-in on a dev box — so without this the
        # suite and a live `FaunaApp.exe --autostart` share one `mls.db`, one daily-rolling
        # `logs/fauna.log.<date>`, one `drafts.json` and one pin store, and a test result
        # depends on what the developer's desktop happens to be doing. `BackupPaths`' own
        # doc comment already states the rule this now enforces: "the real user profile dir
        # is not a root any test may write into."
        #
        # Kept on the driver (not a fresh dir per launch) so `recover()` — which re-posts
        # this same session body — relaunches against the SAME store, preserving the
        # documented windows property that `recover()` is a same-device restart with
        # `mls.db` surviving (unlike linux's fresh-mkdtemp `recover()`). A caller-supplied
        # `config["data_dir"]` still takes precedence and still outlives this instance:
        # the version-skew at-rest grid hands ONE root to a SEQUENCE of driver instances,
        # and `alice_second_device` pins a fresh one for device B.
        #
        # The DEFAULT now hangs off the relocated `LOCALAPPDATA` above, as
        # `<LOCALAPPDATA>\Fauna` — production's own derivation
        # (`BackupPaths.DataDir`), so an isolated launch reproduces the real
        # *relationship* between the two roots (the unified root is `<data dir>\sync`)
        # instead of only isolating them individually. A caller-supplied `data_dir`
        # is honoured verbatim and is NOT re-parented: those two seams hand one root
        # to a sequence of drivers and must keep the exact path they were given, so
        # there the two roots simply stay unrelated — which is why the store root is
        # published separately above rather than derived from the data dir.
        if self._data_dir is None:
            supplied = config.get("data_dir")
            self._data_dir = supplied or os.path.join(self._local_appdata, "Fauna")
            self._owns_data_dir = not supplied
            os.makedirs(self._data_dir, exist_ok=True)
        env["FAUNA_E2E_DATA_DIR"] = self._data_dir
        #: Same contract as `_resolved_credential_dir` above — windows is the one
        #: client whose `launch_config` also has to carry a data dir (its relaunch
        #: is a same-device restart), so an attaching helper needs to read it back.
        self._resolved_data_dir = self._data_dir

        # The seed map is written VERBATIM: the C# store keys its file backend by
        # the logical key itself (`SecretKeyMap` resolves every key verbatim), so
        # `build_registry_seed(...)` output lands exactly where
        # `LogicalSecretStore` looks.
        seed = config.get("seed_credentials")
        if seed:
            cred_file = os.path.join(self._cred_dir, f"{self._keyring_app}.json")
            with open(cred_file, "w") as f:
                json.dump(dict(seed), f)

        self._session_body = {
            "app": config["app_path"],
            "args": app_args,
            "environment": env,
        }
        # A packaged launch (the Store channel's shape): the bridge starts the app in
        # that registered package's context so it runs with the package's identity —
        # a direct start of the exe inside the package has none
        # (`flaui-bridge/SessionManager.cs::StartInPackageContext`). Kept in the
        # session body so `recover()` relaunches the same way.
        if config.get("package_family_name"):
            self._session_body["package_family_name"] = config["package_family_name"]
            self._session_body["package_app_id"] = config.get("package_app_id") or "FaunaApp"
        self._post("/session", self._session_body)
        self._launched_once = True
        # Dismiss any system dialogs (e.g. Windows Firewall) that may block the app
        time.sleep(2)
        self.dismiss_system_dialogs()
        # Wait for TestAgent's poll loop to actually be alive before returning
        # control to the caller: a freshly-spawned FaunaApp.exe is a
        # low-priority background process the OS has not yet scheduled fair CPU
        # time to, and on a busy shared dev machine's ambient load (several
        # cargo/MSBuild jobs routinely running concurrently, sometimes a SECOND
        # windows e2e suite driving its own FaunaApp instance) getting that
        # first slice can genuinely take low double-digit seconds — confirmed
        # live by instrumenting the agent's own poll loop, whose first
        # GET /app/commands round trip was measured anywhere from 950ms to
        # 4.1s depending on load, with zero correlation to the bridge's own
        # request handling. Without this gate, the caller's FIRST real
        # command (e.g. the login patch in set_state()) races that same
        # cold-start variance inside its own fixed 10s ack budget and can
        # lose. This gate absorbs the variance up front, sharing the SAME
        # budget recover() already established for the identical "cold WinUI
        # start under load" problem (e2e-conventions.md § point 14 —
        # "positive waits = named generous budgets + deadline polls; green
        # runs pay nothing") — a healthy launch resolves this on the first or
        # second poll, sub-second.
        # A launch that COLLIDES lands on the account chooser before its
        # TestAgent ever starts (see `_LAUNCH_CHOOSER_ANCHOR_ID`'s comment) —
        # `/app/state` alone would spin this gate's full budget on EVERY such
        # launch, deterministically, not just under load. Accept the chooser's
        # own UIA anchor, read via the bridge that is already up regardless of
        # TestAgent, as an equally valid readiness signal.
        ready_deadline = time.monotonic() + self._RELAUNCH_READY_BUDGET_S
        while time.monotonic() < ready_deadline:
            try:
                resp = self._get("/app/state")
                if resp:
                    break
            except Exception:
                pass
            try:
                if self.is_visible(_LAUNCH_CHOOSER_ANCHOR_ID):
                    break
            except Exception:
                pass
            time.sleep(0.2)
        else:
            raise TimeoutError(
                f"TestAgent poll loop never became responsive within "
                f"{self._RELAUNCH_READY_BUDGET_S:.0f}s of launch (no /app/state "
                f"push observed, and no launch-collision chooser rendered "
                f"either) — the app process may have failed to start, or "
                f"is catastrophically starved of CPU"
            )

    def launch_environment(self) -> dict | None:
        """This driver launches — and relaunches — by POSTing ``_session_body``,
        so its environment is the one the app process holds; there is no
        ``_launch_config`` here to read (``HttpBridgeDriver.launch_environment``
        owns the contract, and ``relaunch_environment`` is that under a relaunch
        gate, so overriding this covers both)."""
        body = self._session_body
        if body is None:
            return None
        environment = body.get("environment")
        return environment if isinstance(environment, dict) else None

    def supports_unclean_kill(self) -> bool:
        """True once `launch()` has posted a session: the FlaUI bridge owns the app
        child, and its `DELETE /session` is a hard `Process.Kill(entireProcessTree:
        true)` (`SessionManager.Quit` — no WM_CLOSE, no app-side cleanup)."""
        return self._session_body is not None

    def kill_uncleanly(self) -> None:
        """Hard-kill the app the bridge tracks (`PlatformDriver.kill_uncleanly`).

        The driver never signals a process itself (process safety): it asks the
        bridge, which owns the handle, to terminate its own child and its tree.
        `?sweep=false` stops ONLY that tracked app — the data-dir sweep is for
        harness leaks, not part of a crash. The bridge verifies the kill and
        answers `closed: false` when the process survived; that is a failed
        primitive, never a silent pass. The next `hard_reload()` → `recover()`
        tolerates the dead app (its own `DELETE /session` is a no-op on none)."""
        if not self.supports_unclean_kill():
            super().kill_uncleanly()  # the NotImplementedError with the contract
        closed = self._delete("/session?sweep=false").get("closed", True)
        if not closed:
            raise RuntimeError("the FlaUI bridge could not kill the app tracked for this session")

    def recover(self) -> bool:
        """Relaunch the FaunaApp child process via the bridge.

        The FlaUI bridge itself stays alive across tests — only the app is
        relaunched. If the bridge's own /health endpoint is refused we give
        up (process gone; a silent-but-listening bridge is stalled, not dead
        — see http_bridge._await_health); restarting the bridge from here
        would require coordinating with the session-scoped driver cache.
        """
        if not self._await_health():
            return False
        if self._session_body is None:
            # launch() was never called; nothing to relaunch
            self._bridge_dead = False
            return True
        self._bridge_dead = False
        # A relaunch that leaves the OLD instance alive is worse than no relaunch:
        # it still holds the per-account instance lock over the data + credential
        # dirs THIS driver deliberately reuses, so the new process takes the app's
        # `[launch-collision] … already served by a live instance` branch, shows the
        # chooser and never starts its TestAgent — every later reset() then dies as
        # "App did not acknowledge reset", and the fixture's retry stacks a third
        # instance. The bridge verifies the kill and 409s rather than launching on
        # top (flaui-bridge/SessionManager.cs::Quit owns the mechanism); surfacing
        # that here as a failed recover() costs ONE legible skip instead of every
        # remaining module in the session.
        sweep_mark = self._untracked_sweep_count()
        try:
            closed = self._delete("/session").get("closed", True)
        except Exception:
            closed = False
        if not closed:
            self._bridge_dead = True
            return False
        # The relaunch restarts the principal-slot carry (convention 10's carve-out,
        # `HttpBridgeDriver._relaunch_principal_slot_carry`). This store survives the
        # relaunch, but the `reset()` that follows every relaunch erases the slot just
        # the same — so harvest it here, with the old process gone and the new one not
        # yet started, for the first sign-in after the relaunch to restore. Once per
        # relaunch, never per reset(): a sign-out → sign-in inside one process mints
        # afresh, as production does.
        self._relaunch_principal_slot_carry()
        # …and the install-device-secret row beside it — same store, same
        # once-per-relaunch harvest, before the erase below wipes it. No
        # dedicated "_relaunch_..." twin exists for the row-shaped carry: unlike
        # the principal slot's, `_begin_install_device_secret_row_carry` takes no
        # config/env to recompute a live decision from (it only harvests + rearms
        # the once-per-launch restore), so calling it again with the SAME
        # resolved store is exactly the relaunch shape.
        self._begin_install_device_secret_row_carry(
            os.path.join(self._cred_dir, f"{self._keyring_app}.json"))
        # …and THEN empty the store the new process will open, so the relaunched
        # app comes back SIGNED OUT — the default relaunch contract every other
        # driver gets for free from a fresh per-launch dir, and the reason
        # convention 10 can say a relaunch is "never a deliberate exercise of the
        # retire arm".
        #
        # Until 2026-09-21 windows relaunched signed IN (this same store, the same
        # session re-posted onto it), which put it alone in reaching the app's
        # retire arm with the `reset()` that follows every relaunch: those e2e
        # arms have been sign-out-shaped since 2026-09-14
        # (`App.xaml.cs::HandleTestCommand` → `DisposeNestClients(signOut: true)`),
        # so the stop retired this machine's enrollment nest-side — the nest
        # vacated the placeholder row and revoked the grant — and the carry then
        # laid the harvested slot back down at the first sign-in, handing the app a
        # credential the nest had already forgotten. Shared Rust's principal
        # succession minted a successor, and the actor collected one fresh `fauna`
        # row per relaunch (`test_relaunch_device_accrual.py --app windows`, red at
        # relaunch 1 of 3). The four fresh-store drivers never reached it: their
        # post-relaunch `reset()` has no live account runtime to retire.
        #
        # So the erase MOVES rather than the carry changing: it happens here, with
        # the old process gone and the new one not yet started, instead of at the
        # reset afterwards with a live runtime attached to it. The carry's harvest
        # above already holds what has to survive, and the restore at the first
        # sign-in is unchanged.
        self._erase_credential_store_for_relaunch()
        # Strip a frozen `--nest-url` before reposting: `launch()`'s own
        # `not self._launched_once` gate only ever fires once, on the driver's
        # FIRST launch() call — but recover() replays that SAME frozen args
        # string on every relaunch after, so a driver whose only launch() was its
        # first (the common case) re-passes --nest-url on EVERY recover() too,
        # reintroducing exactly the override this file's own launch()
        # comment describes: it wins over the vault, so an identity persisted
        # with no nest_url (the abandoned-create-identity scenario) can never
        # take the "(identity, no node_url)" branch on relaunch — a real user
        # restart never receives this flag on any launch, first or later, so
        # recover() shouldn't either. `--secret` stays: a caller-supplied
        # secret_hex names WHICH identity to seed only at cold start, and
        # recover() must keep reusing it identically to a real restart re-
        # reading its own vault.
        recover_body = dict(self._session_body)
        args = recover_body.get("args", "")
        tokens = args.split()
        if "--nest-url" in tokens:
            idx = tokens.index("--nest-url")
            del tokens[idx:idx + 2]
            recover_body["args"] = " ".join(tokens)
        try:
            self._post("/session", recover_body)
        except Exception:
            self._bridge_dead = True
            return False
        # Deadline-poll for the relaunched app's own readiness (convention 14)
        # instead of hoping a flat sleep covers a cold WinUI start under load: the
        # app is ready the moment its agent publishes state, which is exactly what
        # the reset() that follows needs. Green runs pay only the real boot time.
        #
        # The poll's ANSWER is the return value, not a hint. Discarding it — which
        # this did — is the difference between one legible failure and a lost
        # session: an app that never publishes (it died on launch, or it is sitting
        # in the launch-collision chooser, which is reached BEFORE `TestAgent.Start`
        # in `App.xaml.cs`, so its agent never polls and never pushes) would spin
        # the whole budget, be reported ready anyway, and hand the caller an app
        # whose every later `reset()` times out as "App did not acknowledge reset"
        # — a symptom that names the *fixture's* call and not the launch that
        # actually failed. The `app` fixture already routes a False here into a
        # single skip that says the relaunch failed; it just never got one.
        #
        # Budget sized for the answer now being authoritative: a cold WinUI start
        # on a heavily loaded shared build machine (`testing.md`'s load stance) can
        # legitimately take far longer than the old advisory 30 s, and a green run
        # pays only the real boot time either way — so the ceiling is generous and
        # the verdict is honest, rather than tight and ignored.
        # Where the app's own log ends RIGHT NOW, so a failure below can report
        # what the relaunched process wrote rather than what its predecessor
        # left behind. Without this the tail is dominated by the dead instance's
        # retry churn and cannot answer the only question that matters: did the
        # new process log anything at all?
        log_mark = self._app_log_size()
        fence_mark = self._fenced_push_count()
        # A relaunch that only worked because the bridge swept a leaked instance is
        # a PASSING run hiding a real harness defect — the exact shape that let row
        # 39 survive four sessions. The sweep fixes the symptom; this is what keeps
        # the cause visible, naming the survivor and the pid that started it while
        # the run is still green.
        self._report_untracked_sweep(sweep_mark)
        deadline = time.monotonic() + self._RELAUNCH_READY_BUDGET_S
        ready = False
        while time.monotonic() < deadline:
            try:
                if self._get_state_raw():
                    ready = True
                    break
            except Exception:
                pass
            time.sleep(0.2)  # sleep-ok: poll cadence inside a deadline poll
        try:
            self.dismiss_system_dialogs()
        except Exception:
            pass
        if ready:
            # Force the bridge's OWN UIA attach to the freshly-relaunched
            # process's main window to happen HERE, inside this method's
            # spacious 90s budget, instead of leaving it for the caller's
            # first wait_for() (often a tight 10-15s budget) to trigger cold.
            # `_get_state_raw()` above only confirms the APP is alive and
            # publishing over HTTP — it never touches UIA, so it says nothing
            # about whether the bridge (a separate x64 process automating this
            # ARM64 one) has resolved/cached `SessionManager.GetMainWindow`
            # for the NEW process yet. `IsVisible`/`Count` both resolve
            # `RootElement` (which calls `GetMainWindow`) before searching for
            # anything, so this throwaway probe's boolean answer is
            # irrelevant — only the side effect (the bridge attaching once,
            # here) matters. A slow FIRST cross-arch UIA attach on a freshly
            # launched process is a documented, structural flake
            # (`reference_windows_e2e_flake` memory's "ReadProcessMemory 299"
            # entry) —
            # `admin-dashboard-heading` reading `count=0` after a cold
            # relaunch even though the app's own E2eTrace log proves the page
            # was constructed within ~100ms — to exactly this gap. Warming the
            # attach here, where a slow resolve just eats into 90s instead of
            # failing an assertion, is the fix; best-effort, never fails
            # recover() itself.
            try:
                self.is_visible("__row197_uia_attach_warmup__")
            except Exception:
                pass
        if not ready:
            try:
                status = self._get("/session/status")
            except Exception:
                status = {}
            alive = status.get("running")
            # Delta, not the bridge's lifetime total: the fence legitimately
            # discards pushes from an instance killed moments ago, so only what
            # arrived AFTER this relaunch says anything about this relaunch.
            total = status.get("fenced_state_pushes")
            fenced = None if total is None else total - fence_mark
            liveness = (
                "RUNNING (alive but publishing nothing this session can see)"
                if alive
                else "GONE (died during launch)" if alive is False else "UNKNOWN"
            )
            # The fence tally is the discriminator: pushes arriving under a stale
            # epoch mean an agent IS alive and talking, just not this session's —
            # the opposite diagnosis, and the opposite fix, from "no agent at all".
            fence_note = (
                f" {fenced} state push(es) arrived SINCE this relaunch and were "
                f"EPOCH-FENCED (last carried epoch {status.get('last_fenced_epoch')!r} "
                f"vs current {status.get('session_epoch')!r}) — an agent is alive and "
                f"pushing, just not this session's, so a previous instance SURVIVED "
                f"its kill and is still holding this account's instance lock."
                if fenced
                else " No state push arrived at all since the relaunch, fenced or "
                "otherwise — no agent is running anywhere."
                if fenced == 0
                else ""
            )
            # The bridge sweeps the data dir for app processes no handle covers
            # (flaui-bridge/ProcessScan.cs) and kills them before relaunching, so
            # by the time a relaunch fails they are gone — but WHICH ones it had to
            # kill, and who started them, is the evidence for the leak itself. This
            # is the fact four sessions could not see: the epoch fence proves *an*
            # untracked instance existed, this names it and its parent.
            swept = status.get("untracked_survivors") or []
            sweep_note = (
                f" The previous kill left {len(swept)} app process(es) still owning this "
                f"data dir, which the bridge then killed: {'; '.join(swept)}."
                if swept
                else ""
            )
            print(
                f"[e2e] windows: relaunched app published no state within "
                f"{self._RELAUNCH_READY_BUDGET_S:.0f}s. Bridge says the app process is "
                f"{liveness}.{fence_note}{sweep_note} "
                f"Everything the relaunched process wrote to its own log follows "
                f"(data dir {self._data_dir}), then the bridge's own account of the "
                f"kill and the relaunch:"
            )
            print(self._app_log_since(log_mark))
            print(self._bridge_lines())
        return ready

    def _erase_credential_store_for_relaunch(self) -> None:
        """Empty this driver's file-backed credential store, with the old process
        gone and the new one not yet started, so the relaunched app comes back
        signed out.

        windows' spelling of the fresh per-launch store linux, tui, macOS and iOS
        mint inside `launch()`: an isolation axis is per-platform because the
        path derivation is per-platform (`e2e-conventions.md` convention 10), and
        windows' relaunch does not go through `launch()` at all. The DIR itself
        stays — it is named in the frozen `_session_body` this relaunch re-posts,
        and the app's store writes its namespace files on demand.

        Skipped in exactly the two cases where the store is not this driver's to
        empty, and the first is already the carry's own liveness decision
        (`_begin_principal_slot_carry`): a caller who supplied `credential_dir`
        or `keyring_app` owns what rests there — the version-skew at-rest grid
        hands ONE store to a SEQUENCE of driver instances
        (`helpers/skew_client.state_home_config`) — and a test that called
        `preserve_state_across_relaunch()` is asserting client-side durability
        across a restart, which is precisely what the opt-out is for."""
        if not getattr(self, "_principal_slot_carry_live", False):
            return
        if getattr(self, "_preserve_pinned_keys", None):
            return
        cred_dir = getattr(self, "_resolved_credential_dir", None)
        if not cred_dir or not os.path.isdir(cred_dir):
            return
        for name in os.listdir(cred_dir):
            path = os.path.join(cred_dir, name)
            if not os.path.isfile(path):
                continue
            try:
                os.remove(path)
            except OSError as exc:
                # Loud, because the silent version of this is the defect itself:
                # a store that stayed put means the app relaunches SIGNED IN, the
                # reset after it retires this machine's enrollment, and the only
                # symptom is a device-accrual assertion three steps later naming
                # the carry instead of this.
                print(
                    f"[e2e] windows: could not empty the credential store for a "
                    f"relaunch ({path}: {exc}). The relaunched app may come back "
                    f"signed in, and the reset after it would then retire this "
                    f"machine's enrollment nest-side."
                )

    def preserve_state_across_relaunch(self) -> bool:
        """Pin this driver's credential store across the next relaunch, and report
        that it can — the same opt-in linux/tui/macOS/iOS publish, in windows'
        own mechanism.

        windows keeps its credential dir and data dir ON THE DRIVER instance
        rather than minting fresh ones per launch (see `launch()`'s own doc
        comments on `self._cred_dir`/`self._data_dir`), so what a relaunch must
        decide is not WHICH dir the new process gets but whether the old
        process's credentials are still in it. `recover()` empties the store by
        default — a relaunched app comes back signed out, the default relaunch
        contract on every driver (`HttpBridgeDriver.hard_reload`'s docstring) —
        and this pin is what suppresses that erase, so a test asserting
        CLIENT-side durability across a restart gets the app back signed in with
        its own store intact.

        Pinning the two store keys by name (rather than a private windows flag)
        is what makes the rest of the shared machinery work unmodified: the
        pinned `hard_reload()` branch waits for the app's own auto-login instead
        of replaying a login on top of it, and `reset()`'s `_clear_relaunch_pin`
        drops the pin at the per-test boundary, so a durability test cannot leak
        its preserved store into every later relaunch. Before this the predicate
        was a standing `True` that pinned nothing, which meant BOTH of those went
        the wrong way on windows: every relaunch preserved the session whether or
        not any test had asked, and `hard_reload()` — reading an always-empty
        pin set — replayed a login into an app that had already auto-logged-in,
        the second concurrent same-actor login its docstring warns about.

        False only if `launch()` was never called, so there is nothing to
        preserve (mirrors macOS's `self._cred_dir is None` guard)."""
        if self._cred_dir is None or self._data_dir is None:
            return False
        # `_launch_config` here is a stale, never-updated snapshot of the FIRST
        # launch's input (`recover()` relaunches by re-posting `_session_body`,
        # not by re-reading or updating `_launch_config`), so the pin still can't
        # live there. It lives only in `_preserve_pinned_keys` — which
        # `_clear_relaunch_pin` resets whether or not a config dict is there to
        # un-pin keys from.
        self._record_relaunch_pin({}, ("credential_dir", "keyring_app"))
        return True

    def _fenced_push_count(self) -> int:
        """How many state pushes the bridge's epoch fence has discarded so far."""
        try:
            return int(self._get("/session/status").get("fenced_state_pushes") or 0)
        except Exception:  # noqa: BLE001 - a bridge that cannot answer is not a failure here
            return 0

    def bridge_stderr_text(self) -> str:
        """This run's captured FlaUI-bridge stdout+stderr so far ("" if none yet).

        The bridge's analogue of :meth:`app_stderr_text`, and windows needs BOTH
        because the two carry different things. windows' automation server is this
        out-of-process bridge, not an in-app agent, so anything the *automation
        surface* reports — convention 11's `DISABLED-ACTUATION` markers among them
        — is written by the bridge and never appears in the app's own log. linux
        and tui read `app.err` for that same witness only because their agent runs
        inside the app.

        Unfiltered, unlike :meth:`_bridge_lines`: that one keeps the `[bridge] …`
        narration for a failure report, and a caller asserting about a specific
        marker wants the raw stream, including the app's inherited native log.

        Bounded by `drain_pipes`' own deque, so this is a TAIL, not the whole run
        — fine for a test that drives something and reads back immediately, and
        exactly why a sweep harvests `--actuation-log` instead.
        """
        captured = getattr(self, "_bridge_log", None)
        if not captured:
            return ""
        return "\n".join(str(line).rstrip() for line in list(captured))

    def _bridge_lines(self, limit: int = 25) -> str:
        """The bridge's own most recent `[bridge] …` lines.

        `drain_pipes` keeps the bridge's stderr in a bounded deque so it survives
        for diagnostics, but nothing ever read it — so everything the bridge
        reports about a launch or a kill (which pid it started under which epoch,
        which instances it had to sweep, a kill that threw) was written and then
        thrown away. Filtered to the `[bridge]` prefix because the app INHERITS
        these handles and its native tracing shares the stream (`drain_pipes`
        owns that story).

        ``getattr``, not ``self._bridge_log``: every caller is a failure path, and
        a driver whose bridge never started has no buffer at all — the same
        reason ``ack_timeout_diagnostics`` reads it defensively. Attributing a
        crash to ``AttributeError`` *inside the report about it* is the one way
        this method can make a diagnosis worse."""
        captured = getattr(self, "_bridge_log", None)
        if not captured:
            return "(no bridge output captured)"
        lines = self._diagnostic_lines(list(captured))
        return "\n".join(lines[-limit:]) if lines else "(bridge logged nothing)"

    @staticmethod
    def _diagnostic_lines(captured: list[str]) -> list[str]:
        """The `[bridge] …` lines, plus any `[fauna] FATAL` report IN FULL.

        The app's three unhandled-exception hooks (`App.xaml.cs`) write a header
        line and then the exception's whole `ToString()` — a multi-line stack
        trace whose continuation lines carry no prefix at all. Matching per-line
        on a prefix therefore keeps the headline and throws away the only part
        that says WHERE the app died, so a fatal report stays sticky here until
        the app's ordinary timestamped logging resumes.
        """
        out: list[str] = []
        in_fatal = False
        for line in captured:
            stripped = line.rstrip()
            # `drain_pipes` prefixes each line with its stream tag, so match the
            # markers anywhere rather than anchoring at the start.
            #
            # FIRST-CHANCE joins FATAL here because of the one crash class FATAL
            # cannot see: an exception stowed by the WinRT ABI kills the process
            # with 0xC000027B without any terminal event firing, so the throw
            # itself is the only sighting. It is chattier — it also reports
            # exceptions that were handled — which is precisely why it belongs in
            # this post-mortem view and NOT in the live tee.
            if "[fauna] FATAL" in stripped or "[fauna] FIRST-CHANCE" in stripped:
                in_fatal = True
                out.append(stripped)
                continue
            if in_fatal:
                # An ordinary log line (`[err] 2026-09-01T…Z  INFO …`) means the
                # trace is over; anything else is still part of it.
                if _APP_LOG_LINE_RE.search(stripped):
                    in_fatal = False
                else:
                    out.append(stripped)
                    continue
            if "[bridge]" in stripped:
                out.append(stripped)
        return out

    # The bridge's own name for "the app under test is gone", raised by
    # `SessionManager.RefuseIfAppExited` and carried verbatim in the 500 body.
    _APP_EXITED_MARKER = "AppExitedException"

    def bridge_error_diagnostics(self, bridge_error: str) -> str:
        """A bridge error saying the app EXITED carries the app's own last words.

        See the base method for why this hook exists. The windows specifics:

        * **Only an app exit earns it.** Every other bridge 500 — an unresolved
          scope, a bad id — is an ordinary test failure about a live app, and
          dumping the buffer at those would bury the real message under log
          volume on the one path a reader is already struggling with.
        * **``_bridge_lines``, not the raw deque.** The app inherits the bridge's
          stderr, so the deque holds its whole native log; ``_diagnostic_lines``
          keeps the `[bridge]` narration plus each ``[fauna] FATAL`` /
          ``[fauna] FIRST-CHANCE`` report WHOLE (its untagged trace lines
          included) and drops the rest. The generous limit is deliberate: a
          first-chance block is a full stack, and the last one before the death
          is the answer.
        * **No bridge call.** The bridge just told us the app is dead and the
          error already quotes the exit code; asking it again would only add a
          way for this to hang or throw on a failure path.
        """
        if self._APP_EXITED_MARKER not in bridge_error:
            return ""
        return (
            "--- the app's own last words (it inherits the bridge's stderr, so its "
            "unhandled-exception hooks report here; a `[fauna] FIRST-CHANCE` block "
            "is the ONLY sighting of an exception the WinRT ABI stows, and the LAST "
            "one before the death is the candidate) ---\n"
            + self._bridge_lines(limit=200)
        )

    def ack_timeout_diagnostics(self, tail_lines: int = 40) -> str:
        """Whether the app is still ALIVE, and what it last said.

        Convention 6 applied to the ack path — see the base method for why the
        hook exists at all. Windows needs it more than any other driver, because
        here the app can *die* without the driver being able to tell: the app
        child belongs to the FlaUI **bridge**, not to this process, so there is
        no ``Popen`` to ``poll()`` and an abort is indistinguishable from a wedge
        — both are simply "no ack". A live-provisioning session spent two paid Hetzner boxes establishing by hand only that the
        process had died, because this hook returned ``""`` on windows.

        Two facts, both already on hand before this existed, both discarded:

        * **Liveness + exit code**, from the bridge's own tracked handle
          (``SessionManager.AppStatus`` via ``/session/status``). ``0xC0000409``
          is the signature a Rust panic crossing the UniFFI boundary leaves.
        * **The app's own stderr tail.** The app INHERITS the bridge's stdout and
          stderr (``SessionManager.Launch`` starts it ``UseShellExecute=false``
          with no redirect of its own), so its native tracing — a panic message
          and its ``file:line`` included — is drained into ``_bridge_log``.
          ``_bridge_lines()`` filters to ``[bridge]``, which is precisely the
          half the *app* never writes, so this reads the deque UNFILTERED.

        Never raises. It runs inside a ``raise``, so an exception here would
        replace the real failure with its own.
        """
        try:
            parts: list[str] = []

            try:
                status = self._get("/session/status")
            except Exception:  # noqa: BLE001 - a silent bridge is itself the finding
                status = None

            if status is None:
                parts.append(
                    "app process: UNKNOWN — the bridge did not answer "
                    "/session/status (it may have died with the app)"
                )
            elif status.get("running"):
                parts.append(
                    "app process: ALIVE — so this is a genuine wedge inside the "
                    "app, not a crash"
                )
            else:
                parts.append(f"app process: DEAD — {self._exit_code_phrase(status)}")

            tail = [str(ln).rstrip() for ln in (getattr(self, "_bridge_log", None) or [])]
            if tail:
                body = "\n".join(tail[-tail_lines:])
                parts.append(
                    f"--- app + bridge output, last {tail_lines} lines "
                    f"(the app inherits these handles) ---\n{body}"
                )
            else:
                parts.append("(no app/bridge output captured)")

            parts.append(self._rust_log_section(tail_lines))

            return "\n" + "\n".join(parts)
        except Exception as exc:  # noqa: BLE001 - must never mask the real failure
            return f"\n(ack diagnostics unavailable: {type(exc).__name__}: {exc})"

    #: How much of the shared core's `tracing` log to carry, and why it is two
    #: windows rather than one tail. The decisions that name a cause are written
    #: ONCE, at the head of a run (which probe client got built, which reach
    #: address the override took); the evidence that a run is stuck is the LAST
    #: line it managed to write. A single tail loses the first the moment a retry
    #: loop starts producing lines, and a retry loop here runs up to 480 times.
    _RUST_LOG_HEAD_LINES = 60

    def _rust_log_section(self, tail_lines: int) -> str:
        """The shared-Rust half of the same question, kept out of the try above
        so a log that cannot be read costs only this section.

        `_bridge_log` is the app's inherited stdout/stderr; `tracing` does not go
        there. `FaunaFfiMethods.InstallLogging` files it under
        `<data_dir>/logs/fauna.log.<date>`, which :meth:`app_log_text` already
        reads — so everything the shared core narrates about why it is stuck was
        on disk, next to a failure message that never mentioned it.
        """
        try:
            text = self.app_log_text()
        except Exception as exc:  # noqa: BLE001 - never mask the real failure
            return f"(fauna.log unreadable: {type(exc).__name__}: {exc})"
        if not text:
            return "(no fauna.log written — the app never installed Rust logging)"
        lines = text.splitlines()
        tail = tail_lines * 3  # a stuck retry loop narrates one line per attempt
        if len(lines) <= self._RUST_LOG_HEAD_LINES + tail:
            body = "\n".join(lines)
        else:
            body = "\n".join(
                lines[: self._RUST_LOG_HEAD_LINES]
                + [f"    … {len(lines) - self._RUST_LOG_HEAD_LINES - tail} lines elided …"]
                + lines[-tail:]
            )
        return (
            f"--- fauna.log (shared-Rust tracing; first {self._RUST_LOG_HEAD_LINES} "
            f"+ last {tail} of {len(lines)} lines) ---\n{body}"
        )

    @staticmethod
    def _exit_code_phrase(status: dict) -> str:
        """`exit code -1073740791 (0xC0000409) — <what that means>`."""
        raw = status.get("exit_code")
        if raw is None:
            return "the bridge recorded no exit code"
        try:
            code = int(raw)
        except (TypeError, ValueError):
            return f"exit code {raw!r}"
        unsigned = code & 0xFFFFFFFF
        note = _EXIT_CODE_NOTES.get(unsigned)
        phrase = f"exit code {code} (0x{unsigned:08X})"
        return f"{phrase} — {note}" if note else phrase

    def _untracked_sweep_count(self) -> int:
        """How many kills have had to sweep an app instance the bridge never
        tracked (`flaui-bridge/SessionManager.cs::ClearDataDirOwners`)."""
        try:
            return int(self._get("/session/status").get("untracked_sweeps") or 0)
        except Exception:  # noqa: BLE001 - a bridge that cannot answer is not a failure here
            return 0

    def _report_untracked_sweep(self, mark: int) -> None:
        """Print, loudly, any instance THIS relaunch had to sweep.

        Delta against `mark` rather than the bridge's sticky detail list: the
        details persist so a later post-mortem can still read them, which would
        otherwise make every subsequent boundary re-report the same survivor and
        bury the one relaunch that actually leaked it."""
        try:
            status = self._get("/session/status")
        except Exception:  # noqa: BLE001
            return
        if int(status.get("untracked_sweeps") or 0) <= mark:
            return
        print(
            "[e2e] windows: HARNESS LEAK — this relaunch found app process(es) still "
            "owning the session's data dir that the bridge's own handle did not cover, "
            "and killed them so the run could continue: "
            f"{'; '.join(status.get('untracked_survivors') or [])}. The relaunch itself "
            "is fine; the leak is not — the parent pid names whatever started the "
            "untracked instance. The bridge's "
            "own launch/kill record follows, which is what pairs a leaked pid with the "
            "epoch and the tracked pid it was started alongside:"
        )
        print(self._bridge_lines())

    def _app_log_path(self) -> Path | None:
        """The app's current daily-rolling log file, or None if it has none yet."""
        try:
            return max(
                (Path(self._data_dir or "") / "logs").glob("*"),
                key=lambda p: p.stat().st_mtime,
            )
        except Exception:  # noqa: BLE001 - "no log yet" is a legitimate answer
            return None

    def _app_log_size(self) -> int:
        p = self._app_log_path()
        try:
            return p.stat().st_size if p else 0
        except Exception:  # noqa: BLE001
            return 0

    def _app_log_since(self, mark: int, limit: int = 40) -> str:
        """What the app logged after byte offset ``mark``, for a failure to carry.

        Printed INLINE rather than pointed at, because pointing does not survive:
        `teardown()` rmtree's the per-driver `mkdtemp` data dir the log lives in,
        so by the time anyone reads the failure the evidence is gone — which is
        exactly how row 39 stayed unresolved across four sessions
. Convention 6: a failure diagnoses itself.

        Sliced from ``mark`` rather than tailed, because the log is APPEND-SHARED
        across relaunches (same data dir → same daily-rolling file), so a plain
        tail shows the dead predecessor's retry churn and silently answers a
        different question than the one being asked. **Empty output here is the
        strongest possible finding**: `InstallLogging()` is documented as
        literally the first thing the app does, ahead of the window itself, so a
        relaunch that logged nothing never got that far.
        """
        path = self._app_log_path()
        if path is None:
            return f"  (no app log under {self._data_dir}\\logs at all)"
        try:
            with open(path, "rb") as fh:
                fh.seek(mark)
                new = fh.read().decode("utf-8", errors="replace").splitlines()
        except Exception as exc:  # noqa: BLE001 - diagnostics must never mask the failure
            return f"  (app log {path.name} unreadable: {type(exc).__name__}: {exc})"
        if not new:
            return (
                f"  *** {path.name}: the relaunched process wrote NOTHING. It never "
                f"reached InstallLogging(), which the app calls before anything else "
                f"— so it died or blocked during very early startup, not in the UI. ***"
            )
        # Surface the launch-decision lines wherever they sit in the slice. They
        # are written in the first moments of startup and the shared log then
        # buries them under thousands of retry lines from any OTHER live
        # instance, so a plain tail reliably hides the one line that explains the
        # launch — which is precisely how this failure was misread as a stale
        # lock.
        verdicts = [ln for ln in new if "[launch-" in ln or "[instance-lock]" in ln]
        head = (
            "  --- launch decision lines in this slice ---\n"
            + "\n".join("  " + ln for ln in verdicts[:10])
            + "\n"
            if verdicts
            else "  (no [launch-*] / [instance-lock] line in this slice)\n"
        )
        shown = new[-limit:]
        return (
            head
            + f"  --- {path.name}: {len(new)} new line(s), last {len(shown)} ---\n"
            + "\n".join("  " + ln for ln in shown)
        )

    def teardown(self, *, sweep_data_dir_peers: bool = True) -> None:
        """Stop the app and reclaim what this driver owns.

        ``sweep_data_dir_peers=False`` stops ONLY the app this bridge tracks: by
        default the bridge also kills every other instance owning the same data dir
        (a harness leak, `SessionManager.Quit`), which is wrong exactly when the
        sibling is another driver's live app sharing one install world — a test
        closing the first of two colliding instances."""
        try:
            self._delete("/session" if sweep_data_dir_peers else "/session?sweep=false")
        except Exception:
            pass
        self._force_kill()
        # Drop the per-test credential store — but ONLY if this instance created it.
        # A caller-supplied `credential_dir` (the version-skew at-rest grid's
        # state-root-reuse seam) is meant to outlive this one driver instance/process —
        # a later phase launches a FRESH driver against the SAME dir and expects to find
        # what this phase wrote. Best-effort otherwise: a leaked temp dir is noise, a
        # failed teardown is a broken suite.
        if self._cred_dir and self._owns_cred_dir:
            shutil.rmtree(self._cred_dir, ignore_errors=True)
        self._cred_dir = None
        # Same ownership contract for the data dir: a caller-supplied one (the at-rest
        # grid's reuse seam, `alice_second_device`'s device-B root) outlives this driver
        # instance and must survive; the per-instance default is ours to reclaim (an
        # `mls.db` + rolling logs per driver adds up across a full suite).
        #
        # ⚠ The rolling logs it reclaims are the app's OWN Rust log
        # (`<data_dir>/logs/fauna.log.<date>`, filed by
        # `FaunaFfiMethods.InstallLogging`) — the only windows-side record of what
        # the shared crates decided. Reclaiming it at teardown means a FAILING run
        # destroys the evidence for its own failure before anyone can read it, which
        # is precisely how the stalling sweep pass stayed unlocalised across
        # four sessions. `FAUNA_E2E_KEEP_DATA_DIR` keeps it, and prints where.
        # Diagnostic-only and off by default (so the suite still reclaims), same
        # narrow shape as `FAUNA_E2E_AGENT_LOG`: it changes no app behaviour and is
        # read by the HARNESS, never the product.
        keep = bool(os.environ.get("FAUNA_E2E_KEEP_DATA_DIR"))
        if self._data_dir and self._owns_data_dir:
            if keep:
                print(f"[windows-driver] keeping data dir for inspection: {self._data_dir}")
            else:
                shutil.rmtree(self._data_dir, ignore_errors=True)
        self._data_dir = None
        # The relocated `%LOCALAPPDATA%` root, which by default CONTAINS the data dir
        # above (`<LOCALAPPDATA>\Fauna`) plus the unified store root `…\Fauna\sync` — so
        # `FAUNA_E2E_KEEP_DATA_DIR` has to keep this one too, or the diagnostic sweep
        # it exists to prevent would delete the dir it just said it was keeping.
        # Same caller-supplied ownership contract as the two dirs above.
        if self._local_appdata and self._owns_local_appdata and not keep:
            shutil.rmtree(self._local_appdata, ignore_errors=True)
        self._local_appdata = None

    def _force_kill(self) -> None:
        """Kill the bridge process. Safe to call multiple times."""
        if self._bridge_proc:
            self._bridge_proc.terminate()
            try:
                self._bridge_proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self._bridge_proc.kill()
            self._bridge_proc = None

    def start_nest(self, port: int, command: list[str]) -> dict:
        """Start a nest process via the bridge. Returns {port, pid}."""
        return self._post("/nest/start", {"port": port, "command": command})

    def stop_nest(self, port: int) -> dict:
        """Stop a nest process managed by the bridge."""
        return self._delete(f"/nest?port={port}")

    def list_nests(self) -> list[dict]:
        """List all nest processes managed by the bridge."""
        return self._get("/nest/list").get("nests", [])

    def get_clipboard_text(self) -> str | None:
        """Read the Windows clipboard's Unicode text via the FlaUI bridge's
        GET /clipboard/text (raw Win32 OpenClipboard/GetClipboardData — see
        Actions.GetClipboardText for why not the OLE Clipboard wrapper)."""
        return self._get("/clipboard/text").get("text")
