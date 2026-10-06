"""Crash-recovery harness primitives — the adversarial half of the
`nest/common.md` § Client-state recoverability invariant.

The invariant: no state a client can put the nest into is unrecoverable by a
client — EVER, including a client SIGKILL'd at any point of any operation. The
existing `*_survives_app_restart` tests restart the app *gracefully*
(`driver.hard_reload()`); this module supplies the unclean twin:

  - the KILL: ``app.driver.kill_uncleanly()`` (drivers/base.py — SIGKILL the
    driver's OWN app child; process-group, uncatchable, no cleanup handlers);
  - the TIMING: ``NestLogWatch`` tails the nest log for the debug-level
    dispatch beacon (``ws-rpc dispatch received`` — bins/fauna-nest/src/
    dispatch_core.rs) so a kill lands *between* the nest receiving the request
    and the client settling — never a fixed sleep, and the matched line is the
    anti-vacuous proof the operation was genuinely in flight;
  - the RELAUNCH: ``app.driver.hard_reload()`` (existing) — tolerates the dead
    child and replays the injected session, exactly what a crash-relaunch
    needs;
  - the GROUND TRUTH: ``setup_status()`` / a ``WsRpcAdminClient`` side-channel
    read of what the nest actually committed.

The nest-kill half of the harness needs nothing new: ``common.nest.stop_nest(
nest, graceful=False)`` SIGKILLs the nest THIS RUN started and
``start_nest_in_place`` brings it back into the boot reconcile. Both are
mode-agnostic by delegation rather than by branching — ``stop_nest`` rides
``nest["proc"]``, which docker answers with a container adapter, and its partner
asks the handle's own ``start_in_place`` — so in docker the pair is ``docker
kill`` + ``docker start`` on the same container, a harsher restart than
standalone's and the *deployed* one. The third lifecycle, a nest that
``exit(0)``s ITSELF after a factory reset, is ``common.nest.
resume_after_self_exit``: standalone has no supervisor so the harness becomes
one, while in the image s6 already is. (The bridge ``DELETE /nest`` endpoint the
original capture proposed is unnecessary here: the nest is one this run started,
so the unclean kill of an own-child is already safe by construction; the
Windows FlaUI bridge's nest-stop is *already* a hard ``Kill()``.)

Process safety: everything here signals ONLY handles our own fixtures spawned
— never `pkill`/`killall`/name-match (e2e-unified/README.md).
"""

from __future__ import annotations

import os
import re
import time

# The RUST_LOG a dedicated crash nest needs so the dispatch beacon (debug
# level) reaches its log file; production default (`info`) stays quiet.
CRASH_NEST_RUST_LOG = "info,fauna_nest=debug"

# The dispatch-receipt beacon (dispatch_core.rs). `kind=` is the WS-RPC kind.
DISPATCH_BEACON = "ws-rpc dispatch received"

# The fixed e2e device id a `session` patch carries — now owned by
# `helpers.e2e_session` (re-exported here for this module's existing callers).
from helpers.e2e_session import E2E_LOGIN_DEVICE_ID  # noqa: E402


# tracing's fmt layer writes ANSI color codes even into the nest's log FILE;
# strip them before matching or `kind=value` never matches (`kind\x1b[0m…`).
_ANSI_RX = re.compile(r"\x1b\[[0-9;]*m")


class NestLogWatch:
    """Byte-offset-scoped watcher over a nest's log file (ANSI-stripped).

    Construct BEFORE the UI action fires (records the current end-of-file), so
    ``wait_for`` only ever matches lines the action itself produced — a beacon
    from an earlier operation can't satisfy it.

    Mode-blind: ``nest["log_path"]`` is a growing host file in standalone (the
    process's redirected output) and in docker alike (the provider follows the
    container's log into the same ``<tmp_dir>/nest.log``). The offset discipline
    above is exactly why the docker side re-attaches from the restart instant
    rather than replaying — a replayed beacon would satisfy a watch armed for a
    later operation, which is the vacuous kill this class exists to rule out.
    """

    def __init__(self, nest: dict):
        self._path = nest["log_path"]
        try:
            self._offset = os.path.getsize(self._path)
        except OSError:
            self._offset = 0

    def wait_for(self, pattern: str, *, timeout: float = 15.0) -> str:
        """Poll the log for a regex appearing AFTER this watch's offset; return
        the first matching line. Raises TimeoutError with the tail on miss —
        the timing signal must diagnose itself."""
        rx = re.compile(pattern)
        deadline = time.monotonic() + timeout
        appended = ""
        while time.monotonic() < deadline:
            try:
                with open(self._path, "r", errors="replace") as fh:
                    fh.seek(self._offset)
                    appended = fh.read()
            except OSError:
                appended = ""
            appended = _ANSI_RX.sub("", appended)
            for line in appended.splitlines():
                if rx.search(line):
                    return line
            time.sleep(0.05)
        tail = "\n".join(appended.splitlines()[-15:])
        raise TimeoutError(
            f"nest log never matched {pattern!r} within {timeout:.0f}s of the "
            f"watch point — the operation under test may never have reached "
            f"the nest (vacuous kill). Log since watch point (tail):\n{tail}"
        )

    def wait_for_dispatch(self, kind: str, *, timeout: float = 15.0) -> str:
        """Wait for the dispatch-receipt beacon of a specific WS-RPC kind —
        the sanctioned kill point: the nest HAS the request, the client has
        not settled."""
        return self.wait_for(
            rf"{DISPATCH_BEACON}.*kind[=:]\s*{re.escape(kind)}"
            rf"|kind[=:]\s*{re.escape(kind)}.*{DISPATCH_BEACON}",
            timeout=timeout,
        )


def setup_status(nest_url: str) -> dict:
    """Anon side-channel read of `fauna.setup.status` → {claimed, admin_exists}.
    The claim-state ground truth (keyed on an admin row existing — nest
    common.md § Implementation status)."""
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(nest_url) as anon:
        return anon.call("fauna.setup.status", {})


# How many registry ids the native dump prints before it summarizes the rest.
# A stuck launch surface carries a handful; a rendered shell carries dozens,
# and the count alone already separates them.
_DUMP_ID_CAP = 60


def launch_surface_dump(app) -> str:
    """Diagnostic snapshot of a stuck launch surface, for a failure message.

    A launch page that renders `Loading…` forever fails as a bare `count=0` on
    whatever the journey waited for — and that single number is equally
    explained by "the slot did not survive the kill", "the page never booted",
    and "the reader missed a slot that is sitting right there". Only the live
    DOM plus the very store the reader reads separates them, so a wait on the
    launch path puts this in its own failure text rather than leaving the next
    session to reproduce it (testing.md § point 6 — failures must diagnose
    themselves; twice now this spot has produced a confident wrong root cause
    for want of exactly this).

    Two arms, because the two app families publish different surfaces — but
    NEITHER is empty. Web reads the live DOM + `localStorage` through `eval_js`;
    every other app reads its own state snapshot and its automation registry
    (`registry_snapshot`, the structured "what is on screen right now").

    ⚠ The native arm exists because this returned a bare `""` on every non-web
    driver until 2026-09-21, and a caller that interpolates it — every caller
    does — then reads as though it had LOOKED and found nothing. A macOS red
    here was diagnosed as "the app is on neither the admin shell nor any launch
    surface" off exactly that empty string, which the dump had never been in a
    position to say (`e2e-conventions.md` point 6 — and the third confident
    wrong root cause this spot has produced).
    """
    eval_js = getattr(app.driver, "eval_js", None)
    if eval_js is None:
        return _native_launch_surface_dump(app)
    try:
        dump = repr(eval_js("""(() => ({
          href: location.href,
          text: (document.body && document.body.innerText || '').slice(0, 160),
          testids: Array.from(document.querySelectorAll('[data-testid]'))
                        .map(function (e) { return e.dataset.testid; }),
          store: Object.assign({}, localStorage),
        }))()"""))
    except Exception as exc:  # a dump must never mask the failure it describes
        dump = f"<launch-surface dump failed: {exc!r}>"

    # The browser console + pageerror ring the bridge captured (web driver
    # only). This is the ONLY witness of an unhandled rejection inside the
    # SPA's async onMount launch sequence — the DOM/store above cannot
    # distinguish "a reader returned null" from "a step threw" (both leave the
    # bare loader), but a throw lands here as a `[pageerror]` line.
    console_log = getattr(app.driver, "console_log", None)
    if console_log is not None:
        try:
            lines = console_log()
            tail = "\n".join(lines[-40:]) if lines else "<empty>"
            dump += f"\nbrowser console (last {min(len(lines), 40)} of {len(lines)}):\n{tail}"
        except Exception as exc:
            dump += f"\n<console capture failed: {exc!r}>"
    return dump


def _native_launch_surface_dump(app) -> str:
    """`launch_surface_dump`'s arm for the apps with no JS context.

    The two questions a stuck native launch poses are "where does the app think
    it is" and "what did it actually put on screen", and they are answered by
    different surfaces: the state snapshot carries `nav` + `session` (what the
    shell believes), the registry carries the ids (what a driver lookup would
    have found). A shell sitting on a launch surface shows it in `nav`; one that
    rendered nothing at all shows an EMPTY registry, which is a different
    finding and must not read the same.

    `registry_snapshot()`'s `None`/`[]` distinction is preserved verbatim
    (`drivers/base.py` — they are opposite answers), and ids are summarized
    rather than dumped whole: the list is the signal, the per-record attributes
    are `driver.diagnose`'s job.
    """
    parts: list[str] = []
    try:
        state = app.driver.get_state() or {}
        parts.append(f"nav={state.get('nav')!r}")
        parts.append(f"session={state.get('session')!r}")
        parts.append(f"state keys={sorted(state)!r}")
    except Exception as exc:  # a dump must never mask the failure it describes
        parts.append(f"<state read failed: {exc!r}>")
    try:
        records = app.driver.registry_snapshot()
        if records is None:
            parts.append("registry=<app publishes no registry surface>")
        else:
            ids = sorted({str(r.get("id")) for r in records})
            shown = ids[:_DUMP_ID_CAP]
            more = f" (+{len(ids) - len(shown)} more)" if len(ids) > len(shown) else ""
            parts.append(f"registry ids ({len(records)} records): {shown}{more}")
    except Exception as exc:
        parts.append(f"<registry read failed: {exc!r}>")
    return "\n".join(parts)


# The admin-dashboard nav patch. The dashboard sub-page is named explicitly
# rather than left to a bare {"view": "admin"} resolving to it: it does resolve
# on macOS/Windows, but on a LIVE app already sitting on another admin sub-page
# the shells used to disagree, so the readiness probe silently encoded macOS's
# semantics; the nest-kill journey (the one journey that leaves the client alive)
# was red on linux for exactly that reason. Linux now defaults a bare admin nav
# to the dashboard like the others, and this stays explicit so the probe never
# depends on that default again.
_ADMIN_DASHBOARD_NAV = {"stack": [{"view": "admin"}, {"view": "admin", "id": "dashboard"}]}


def inject_admin_session(app, nest_url: str, admin_secret_hex: str, *,
                         torn_store_resilient: bool = False) -> None:
    """Point the (re)launched client at a dedicated nest as its admin and land
    on the admin shell — the same `session` patch `admin_app`/`logged_in_app`
    inject (fixture setup, not the mutation under test). Also becomes the
    driver's ``_last_session``, so a later ``hard_reload()`` replays it. A
    journey relaunches the app seeded for that nest before its first call here
    (``conftest._relaunch_trusting_nest``) — this helper never relaunches, since
    after a crash a relaunch would erase the state under test.

    ``torn_store_resilient`` (the mid-claim journey): a client SIGKILL'd
    mid-claim leaves the web account registry holding an UNBOUND identity (the
    imported secret, never bound to a nest — the per-actor `nest_url` lands
    only at claim success, which the kill beat). Clearing the crash-torn
    registry first (`driver.reset()`) lets this inject rebuild the account WITH
    the nest binding, so the session sticks and the admin shell holds — a
    faithful "relaunched client re-establishes its admin session" recovery
    over the very path the `admin_app` fixture proves on a fresh store.
    The box's claimed+admin state (the invariant actually under test) is asserted
    server-side by the caller, independent of the client store, so the reset does
    not make the assertion vacuous (contrast CR-1, journey 4, whose assertion IS
    the preserved client slot)."""
    if torn_store_resilient:
        app.driver.reset()
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest_url,
            "secret_hex": admin_secret_hex,
            "device_id": E2E_LOGIN_DEVICE_ID,
            # Matches the "admin" handle convention used elsewhere for an injected
            # admin session (e.g. test_account_switcher_*.py). A real admin who has
            # used the app normally always has a cached display handle by the time
            # they reach the factory-reset button; without one here, apple's
            # `AdminNestVM.factoryReset()` mint has no fallback source once its own
            # live `getAccount()` call fails (the within_grace journey kills the nest
            # before the click specifically so that call fails), and the resume lands
            # on "a handle is required to claim the nest" instead of exercising the
            # already-claimed rejection the journey asserts on.
            "handle": "admin",
        },
        "nav": _ADMIN_DASHBOARD_NAV,
    })
    try:
        app.driver.wait_for("admin-dashboard-heading", timeout=20.0)
    except Exception as exc:
        # Journey 3's recorded failure point — self-diagnose it the same way
        # journey 4's wait does (testing.md § point 6), so a red here carries
        # the DOM/store/console evidence instead of a bare count=0.
        detail = (" (after clearing the crash-torn registry)"
                  if torn_store_resilient else "")
        raise AssertionError(
            f"the injected admin session never reached the admin dashboard{detail}. "
            f"error={app.error_text()!r} launch surface: {launch_surface_dump(app)}"
        ) from exc
