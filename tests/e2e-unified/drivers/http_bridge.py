from __future__ import annotations

import json
import os
import shutil
import signal
import sys
import tempfile
import time
import traceback
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

from .base import PlatformDriver
from .scope import parse_scope, scope_to_wire

SCREENSHOT_DIR = Path(__file__).parent.parent / "screenshots"

# Socket ceiling for one bridge RPC (testing.md § Cross-app e2e conventions,
# point 14: a named generous budget sized far above any non-pathological delay —
# a healthy bridge answers in milliseconds, so a green run never pays it; only a
# genuinely slow call waits, still bounded per convention 9). Deliberately ONE
# shared constant rather than per-call-site bumps: after a navigation the boot
# cost lands on whichever cheap call happens to run next (`_ensure_agent`'s
# trivial typeof probe inherits the whole wasm reboot), so no per-site list can
# ever be complete. Resolved at call time (`timeout=None` → this), so tests can
# monkeypatch it.
BRIDGE_RPC_TIMEOUT_S = 120.0

# After an RPC socket timeout, how long /health gets to answer cleanly before
# we settle for the weaker listener-level evidence. A timeout means the server
# ACCEPTED the connection (or at worst its accept backlog is full) — evidence
# of life under machine load, not death. The bridge servers are single-threaded
# (web-bridge/server.py uses plain HTTPServer), so the request we abandoned
# keeps running server-side and /health cannot answer until it drains — a
# one-shot probe here structurally observes the same stall twice and calls it
# death; worse, a page-bound Playwright call issued while the SPA reboots wasm
# under machine load can peg the bridge thread for the WHOLE boot (minutes), so
# even a generous poll can expire with health still silent. Silence therefore
# never concludes death: on loopback a connect to a dead port is REFUSED
# instantly, so "probes time out" ⇒ the listener accepted them ⇒ the process
# is alive. Only a refused/reset connection ever concludes death.
BRIDGE_DEATH_CONFIRM_BUDGET_S = 90.0

# Socket timeout for a single /health probe (one iteration of the poll above,
# and the base recover() probe).
BRIDGE_HEALTH_PROBE_TIMEOUT_S = 5.0

# How long a posted command gets to be ACKNOWLEDGED — the app echoing our
# `last_command_id` back on `/app/state` (testing.md § Cross-app e2e conventions,
# point 14). One named generous budget for all three ack call sites (`set_state`,
# `call_machine_method`, `call_command`), resolved at call time (`timeout=None` →
# this) so tests can monkeypatch it.
#
# WHY GENEROUS COSTS NOTHING. Every ack site is a deadline POLL that returns the
# instant the ack lands, so a green run pays the real ack latency and not one ms
# of this ceiling. The budget is only ever spent on a run that was going to fail;
# raising it cannot slow a passing suite down.
#
# WHY IT WAS WRONG BEFORE (measured 2026-08-16). These three sites are the
# SAME mechanism — post a command, poll for its id — yet `call_command` carried a
# bare `5.0` while its two siblings carried `10.0`, a split with no stated reason
# that made the flakiest path the tightest one. Both of the reds were ack
# timeouts on a loaded box, and the phase timings say why: the app logged
# `serialize=2102ms post=488ms` for a single push under load, where serialize is
# in-process state marshalling — pure CPU, nothing to do with agent IPC. On a
# build machine running many concurrent builds a CPU-starved app thread can lose
# seconds at a time, so a 5 s ceiling was a latent flake generator for EVERY
# windows suite, not just the one that happened to catch it. The same run measured
# `provider=0-26ms serialize=0-24ms post=1-12ms` on a quiet-ish box — a ~48x
# spread between quiet and loaded, which is exactly the spread convention 14 says
# a budget must clear rather than a number a green run must be lucky to meet.
#
# 60 s is ~23x the worst push ever observed here and still an order of magnitude
# inside the per-test `timeout = 900` (convention 9), so a genuinely wedged app
# still fails bounded and self-diagnosing rather than hanging the suite.
BRIDGE_ACK_BUDGET_S = 60.0

# How many PEGGED verdicts in a row (`_timeout_verdict`'s last branch: the RPC
# timed out and /health stayed silent for the whole confirm budget) end the RUN.
# "In a row" means the bridge answered nothing between them — any HTTP answer,
# error codes included, and any clean /health probe restart the count
# (`_bridge_answered`). Classifying a pegged thread as alive is right for ONE
# call (an SPA boot under load outlasts any polite budget), but a thread that
# never frees turns every later test into a full `HEAVY_BOOT_TIMEOUT_S` + confirm
# wait, and the run never recovers: measured 2026-09-30 on a web tier_2/3 run,
# nine tests in a row each paid ~490 s, with ~467 still to go — days, not hours,
# while holding the one-wide `e2e_other` lane. Three
# is ~25 minutes of a bridge answering nothing, far past any boot ever measured
# (convention 14's generous-budget rule), so a run that reaches it is aborted
# with `pytest.exit` — the harness's fail-fast shape for a degraded environment
# (conftest's apple render preflight) — rather than left to burn its slot.
BRIDGE_PEGGED_STREAK_ABORT = 3


class BridgeDead(Exception):
    """Raised when the bridge process has died and can't be reached."""
    pass


# The greppable fragment `fauna_e2e_agent::disabled_actuation_refusal` opens
# every disabled-actuation refusal with. Both 409-answering refusals a `select`
# can meet share a status code, so the message is the only discriminator — see
# `select()` below.
_DISABLED_ACTUATION_REFUSAL = "element is disabled"


class SelectOptionNotOffered(RuntimeError):
    """`select(id, value)` asked for an option the app never rendered.

    The refusal is the app's, not the driver's: an agent that finds the picker
    but cannot find `value` among the options *this frame painted* answers 409
    rather than actuating (`e2e-conventions.md` § convention 11 — a picker
    refuses a value the frame did not offer). Driving a picker to a state no
    keystroke can reach tests something no user can do, and greens while the
    option list itself is broken.

    Subclasses `RuntimeError` so the pre-existing generic bridge-error handling
    (and every `pytest.raises(Exception)`) still catches it — the type is here so
    a test can assert *this* refusal specifically, and so the traceback names the
    contract instead of reading as a transport fault.
    """


class HttpBridgeDriver(PlatformDriver):
    """PlatformDriver backed by an automation bridge HTTP server.

    Subclasses override launch() and teardown() to manage the bridge process.
    All element operations are forwarded to the bridge via HTTP.
    """

    def __init__(self, bridge_url: str = ""):
        self._url = bridge_url
        self._bridge_dead = False
        # Last `session` block injected via set_state — the authenticated identity
        # (secret_hex/node_url/actor_id). Cached so an in-process `hard_reload`
        # (force-quit + relaunch) can re-establish the SAME actor; the in-process
        # app holds identity in process memory, so a relaunch needs it replayed.
        self._last_session = None

    _recover_in_progress = False

    def ack_timeout_diagnostics(self, tail_lines: int = 40) -> str:
        """What the APP said while it was failing to ack, for a timeout message.

        A command that never acks is the least diagnosable failure this harness
        produces: the driver knows only "no ack", and the app's own log — which
        names the phase it wedged in — sits in a temp file nobody reads. A prior
        investigation cost three full runs establishing only that a
        never-acking ``conversations_real_resolve_send_new`` was *not* a tight
        budget, because every one of them threw this away. Convention 6 (a
        failure must diagnose itself) applied to the ack path.

        Default is empty — a driver whose app writes no log it can reach adds
        nothing to the message. Overridden where the log exists (``linux``).
        """
        return ""

    def bridge_error_diagnostics(self, bridge_error: str) -> str:
        """What the APP said on its way out, for a bridge error that reports it dead.

        The sibling of ``ack_timeout_diagnostics``, for the failure shape a
        *better* bridge produces. A bridge that notices the app has exited and
        refuses the next query outright — rather than waiting out a budget the
        corpse will never satisfy — turns a crash into an ordinary HTTP 500, and
        the 500 path called no diagnostics hook at all. So the more responsive the
        bridge got, the less the crash said: the failure quoted the BRIDGE's
        stack, while the app's own report sat unread in the driver's buffer.

        Measured 2026-09-09 on Windows: the app
        died ``0xC000027B`` (a WinRT-stowed exception), its ``[fauna] FATAL``
        header and every ``[fauna] FIRST-CHANCE`` block — the only sighting such
        an exception ever gets — were captured and discarded, and the reported
        failure was seven frames of ``FauiBridge``. Convention 6 applied one layer
        out from the ack path.

        Returns text appended to the raised ``RuntimeError``. Default is empty;
        it is called on EVERY bridge error, so an override must add nothing
        unless the error genuinely says the app is gone. Like
        ``ack_timeout_diagnostics`` it runs inside a ``raise`` and must never
        raise itself — ``_bridge_error_note`` backstops that, but an override
        leaning on the backstop puts a stray ``AttributeError`` where the
        diagnosis belongs, which is the failure it exists to prevent.
        """
        return ""

    def _bridge_error_note(self, msg: str) -> str:
        """``bridge_error_diagnostics``, with the never-raises guarantee enforced
        HERE rather than trusted to each override. This sits on the failure path of
        every bridge call: an override that throws would replace the real failure
        with its own, which is precisely the diagnosis being rescued."""
        try:
            note = self.bridge_error_diagnostics(msg)
        except Exception as exc:  # noqa: BLE001 - see the docstring
            return f"\n[diagnostics unavailable: {type(exc).__name__}: {exc}]"
        return f"\n{note}" if note else ""

    def _mark_dead(self, method: str, path: str, exc: BaseException) -> None:
        """Set _bridge_dead and print a diagnostic so we can find the culprit."""
        self._bridge_dead = True
        test = os.environ.get("PYTEST_CURRENT_TEST", "?")
        print(
            f"\n[BRIDGE DEAD during {test}] {method} {path}: "
            f"{type(exc).__name__}: {exc}",
            file=sys.stderr,
        )

    @staticmethod
    def _is_timeout(exc: BaseException) -> bool:
        """True when exc is a socket timeout — raw ``TimeoutError`` (read
        phase: the server accepted and is still working) or a ``URLError``
        wrapping one (connect phase: the accept backlog is full — the
        listener exists but is swamped). Both are evidence of life under
        load. A refused/reset connection is never a timeout: the peer
        actively signalled there is nothing there."""
        if isinstance(exc, TimeoutError):
            return True
        return isinstance(exc, urllib.error.URLError) and isinstance(
            exc.reason, TimeoutError
        )

    def _probe_health(self, timeout: float | None = None) -> str:
        """One /health probe, classified: ``"ok"`` (any HTTP answer with
        ready not false), ``"refused"`` (connection refused/reset — the
        peer actively signalled there is nothing there), or ``"silent"``
        (the connect was accepted but no answer arrived in time — the
        listener exists, so the process is alive but its thread is busy).
        On loopback a connect to a dead port is refused instantly, which is
        what makes "silent" reliable evidence of life rather than mere
        absence of evidence."""
        if timeout is None:
            timeout = BRIDGE_HEALTH_PROBE_TIMEOUT_S
        try:
            resp = urllib.request.urlopen(f"{self._url}/health", timeout=timeout)
            body = resp.read()
            if not body:
                return "ok" if resp.status == 200 else "silent"
            data = json.loads(body)
            return "ok" if bool(data.get("ready", True)) else "silent"
        except (ConnectionRefusedError, ConnectionResetError):
            return "refused"
        except urllib.error.URLError as e:
            if isinstance(e.reason, (ConnectionRefusedError, ConnectionResetError)):
                return "refused"
            return "silent"
        except Exception:
            return "silent"

    def _await_health(self, budget: float | None = None) -> bool:
        """Deadline-poll /health under ``budget`` (default
        BRIDGE_DEATH_CONFIRM_BUDGET_S). True as soon as a probe answers
        cleanly, False the moment one is refused (process gone). Expiry with
        only silent probes is True: every silent probe's connect was
        accepted, so the listener — hence the process — exists; it is
        stalled (typically a page-bound call pegged by an SPA boot under
        machine load), not dead."""
        if budget is None:
            budget = BRIDGE_DEATH_CONFIRM_BUDGET_S
        deadline = time.monotonic() + budget
        while time.monotonic() < deadline:
            verdict = self._probe_health()
            if verdict == "ok":
                return True
            if verdict == "refused":
                return False
            time.sleep(1.0)  # sleep-ok: poll cadence inside a deadline poll
        return True

    def _timeout_verdict(self, method: str, path: str, exc: BaseException,
                         timeout: float) -> None:
        """Decide what a timed-out RPC means: alive-but-slow, or dead.

        A socket timeout proves the server accepted the connection (see
        `_is_timeout`), so it must NOT be conflated with a refused/reset
        connection — that conflation is what turned one slow response under
        machine load into a whole-session skip (the false "[BRIDGE DEAD …
        (setup)]" family). Deadline-poll /health under a named budget — the
        single-threaded bridge server first has to finish the request we
        abandoned, so a one-shot probe would just observe the same stall
        twice. Silence at budget expiry still classifies as ALIVE (see
        `_await_health`: the accepted connects prove the listener exists —
        the bridge thread is pegged, typically inside a page-bound Playwright
        call waiting out an SPA wasm reboot under machine load, which can
        outlast any polite budget). Death is concluded ONLY from a refused/
        reset probe.

        Never returns: raises plain ``TimeoutError`` when the bridge is
        alive (callers retry — the conftest ``app`` fixture retries
        ``reset()`` once on exactly this), or marks dead and raises
        ``BridgeDead`` when the process is really gone (bounded either way,
        convention 9).
        """
        deadline = time.monotonic() + BRIDGE_DEATH_CONFIRM_BUDGET_S
        while time.monotonic() < deadline:
            verdict = self._probe_health()
            if verdict == "ok":
                self._bridge_answered()
                raise TimeoutError(
                    f"{method} {path} timed out after {timeout}s, but the "
                    f"bridge is alive (/health answered — slow under machine "
                    f"load, not dead)"
                ) from exc
            if verdict == "refused":
                self._mark_dead(method, path, exc)
                raise BridgeDead(
                    f"Bridge died: {method} {path} timed out and a /health "
                    f"probe was then refused (process gone): {exc}"
                ) from exc
            time.sleep(1.0)  # sleep-ok: poll cadence inside a deadline poll
        # Every probe's connect was accepted — the listener exists, the
        # process is alive, its thread is just pegged. Say so on stderr
        # (visible under -s / on errors) and classify alive.
        test = os.environ.get("PYTEST_CURRENT_TEST", "?")
        print(
            f"\n[BRIDGE STALLED during {test}] {method} {path}: timed out "
            f"after {timeout}s and /health stayed silent for "
            f"{BRIDGE_DEATH_CONFIRM_BUDGET_S}s, but the listener is still "
            f"accepting connects — bridge thread pegged (SPA boot under "
            f"load?), NOT dead",
            file=sys.stderr,
        )
        self._note_pegged(method, path, timeout)
        raise TimeoutError(
            f"{method} {path} timed out after {timeout}s; /health silent for "
            f"{BRIDGE_DEATH_CONFIRM_BUDGET_S}s but the listener still accepts "
            f"connects — bridge thread pegged, not dead"
        ) from exc

    def _bridge_answered(self) -> None:
        """The bridge thread answered something: a pegged streak is over."""
        self._pegged_streak = 0

    def _note_pegged(self, method: str, path: str, timeout: float) -> None:
        """Count one pegged verdict; abort the run at BRIDGE_PEGGED_STREAK_ABORT.

        An abort, not a skip or a `BridgeDead`: the `app` fixture answers both
        with `recover()` + a second `reset()`, which on a pegged bridge is two
        more full waits per test — the very streak being bounded. The message
        names the streak and the test it ended in, and the non-zero exit code
        makes the run's verdict red (the gate's INFRA classification is for a
        run that never started, and this one did)."""
        self._pegged_streak = getattr(self, "_pegged_streak", 0) + 1
        if self._pegged_streak < BRIDGE_PEGGED_STREAK_ABORT:
            return
        import pytest

        test = os.environ.get("PYTEST_CURRENT_TEST", "?")
        pytest.exit(
            f"[BRIDGE PEGGED STREAK] {self._pegged_streak} bridge RPCs in a row "
            f"timed out with /health silent and no answer between them (last: "
            f"{method} {path} after {timeout}s, during {test}) — the bridge "
            f"thread is not freeing, so every later test would pay the same "
            f"wait; aborting the run (http_bridge.BRIDGE_PEGGED_STREAK_ABORT)",
            returncode=3,
        )

    def _check_bridge(self) -> None:
        """Raise BridgeDead if the bridge has been detected as dead.

        Attempts one in-line recovery first. If recover() succeeds, the
        caller can proceed. If it fails, BridgeDead is raised so the test
        layer can skip the remainder of the session.
        """
        if not self._bridge_dead:
            return
        if self._recover_in_progress:
            # recover() itself is running and made an HTTP call — just raise
            # so we don't recurse.
            raise BridgeDead("Bridge dead during recovery")
        self._recover_in_progress = True
        try:
            if not self.recover():
                raise BridgeDead("Bridge previously marked dead; recover() failed")
        finally:
            self._recover_in_progress = False

    def _health_raw(self, timeout: float | None = None) -> bool:
        """Probe /health without tripping the _bridge_dead guard.

        Returns True if the bridge HTTP server responds, regardless of
        whether the driver believes the bridge is dead.
        """
        if timeout is None:
            timeout = BRIDGE_HEALTH_PROBE_TIMEOUT_S
        try:
            resp = urllib.request.urlopen(f"{self._url}/health", timeout=timeout)
            body = resp.read()
            if not body:
                return resp.status == 200
            data = json.loads(body)
            return bool(data.get("ready", True))
        except Exception:
            return False

    def recover(self) -> bool:
        """Attempt to clear the _bridge_dead flag so the driver is usable again.

        Base behaviour: if /health answers within the patience budget — or
        stays silent while still accepting connects (stalled, not dead; see
        `_await_health`) — clear the flag and return True. A refused probe
        returns False fast (first probe): the process is gone. Subclasses
        override to also relaunch the app process (the bridge may be alive
        while the app under test has crashed or is stuck).

        Returns True on success, False if the bridge is truly dead and the
        caller should give up on this driver for the rest of the session.
        """
        if self._await_health():
            self._bridge_dead = False
            return True
        return False

    def supports_cold_relaunch(self) -> bool:
        """True when this driver's recover() actually relaunches the app under
        test. The base recover() above only health-probes the bridge and clears
        `_bridge_dead` — useful for crash recovery, not a relaunch — so the
        capability is exactly "does the class override recover()": derived from
        the MRO rather than tabulated per app (convention 7's
        `app_capabilities.py` lesson — a hand-kept table answers wrongly for
        a driver it never heard of), and equality-pinned per driver by
        `tests/test_module_relaunch.py`. Today only android inherits the base
        (its bridge keeps no relaunchable session).
        Consumed by the module-boundary relaunch contract
        (`helpers/module_relaunch.py`, testing.md § point 10)."""
        return type(self).recover is not HttpBridgeDriver.recover

    def launch_environment(self) -> dict | None:
        """The environment THIS launch handed the app process, as the live dict —
        or ``None`` for a driver whose launch carries none (web, whose browser
        page reads no process environment).

        **Where a driver keeps the AUTHORITATIVE copy of that dict is a
        per-driver fact, and this is the one place each driver states it.** Most
        relaunch from ``_launch_config`` and so read it there; windows ALSO
        keeps a pristine ``_launch_config`` (for a caller that needs to relaunch
        from a modified copy of its own original config, e.g.
        ``test_add_account_provisioning.py``), but its authoritative copy is
        the bridge ``_session_body`` `recover()` re-posts on relaunch, and this
        method overrides accordingly. Every question of the form *"did this
        launch pin X?"* asks here rather than reaching into either attribute —
        a caller that reached into ``_launch_config`` directly silently
        answered ``None`` on windows, which is how the named-row barrier came
        to skip every windows run whatever the launch had pinned."""
        config = getattr(self, "_launch_config", None)
        if not isinstance(config, dict):
            return None
        environment = config.get("environment")
        return environment if isinstance(environment, dict) else None

    def relaunch_environment(self) -> dict | None:
        """The launch environment this app's NEXT ``recover()`` starts with, as the
        live dict that relaunch will read — or ``None`` when no relaunch re-reads
        one.

        What the e2e trust seed's re-point rule needs
        (``conftest._relaunch_trusting_nest``; ``e2e-automation-surface-gating.md``
        § The e2e trust seed): a process environment is fixed at launch, so a nest
        the launch did not name can be trusted only by a relaunch that names it.
        ``None`` on a driver that cannot relaunch at all
        (``supports_cold_relaunch``), and on one whose launch carries no
        environment — web, whose browser page reads none.

        It is :meth:`launch_environment` under a relaunch gate, and nothing
        more: a relaunch re-reads the dict the launch was given (windows
        re-POSTs the very ``_session_body`` it launched from), so the two
        questions differ only in whether a relaunch happens at all. Keeping the
        storage detail in one method is the point — this used to carry its own
        copy, and windows its own override of that copy.
        """
        if not self.supports_cold_relaunch():
            return None
        return self.launch_environment()

    #: The ``S.region.*`` key naming the source this app's platform leaf reports
    #: for its declared region (``region-blocking.md`` § Region determination):
    #: the OS's user-set region setting unless a driver says otherwise. The e2e
    #: declared-region override keeps the leaf's source, so this is what the
    #: settings surface shows under test too.
    REGION_SOURCE_KEY = "source_system_region"

    #: The test-capable-build seams ``fauna_client_region`` reads (convention 15):
    #: the region registry seed, and the declared-region override.
    _REGION_ENV_KEYS = ("FAUNA_E2E_REGION_REGISTRY", "FAUNA_E2E_REGION_DECLARED")

    def declare_region_for_relaunch(self, region: str, registry_hex: str) -> bool:
        """Arrange for the NEXT ``recover()`` to launch the app declared in the
        synthetic ``region``, trusting the test-only authority ``registry_hex``
        enrols — the way a journey reaches a region a test cannot set on the host
        (a storefront, a user geo). One uniform seam every app reads through the
        shared plane (``fauna_client_region::source::with_e2e_override``), so no
        app grows its own. False when the driver cannot relaunch with an
        environment (the caller skips rather than passing vacuously)."""
        env = self.relaunch_environment()
        if env is None:
            return False
        registry_key, declared_key = self._REGION_ENV_KEYS
        env[registry_key] = registry_hex
        env[declared_key] = region
        return True

    def clear_region_declaration(self) -> None:
        """Undo :meth:`declare_region_for_relaunch` for the next relaunch."""
        env = self.relaunch_environment()
        if env is None:
            return
        for key in self._REGION_ENV_KEYS:
            env.pop(key, None)

    def relaunch_preserves_injected_identity(self) -> bool:
        """True when a bare ``recover()`` leaves the app authenticated as the actor
        a prior ``set_state`` session injected — i.e. the app signs ITSELF back in
        after the relaunch, with no replay from the test.

        True on the native drivers: ``logged_in_app``'s injected session makes the
        app write real credentials to its own store, and
        ``preserve_state_across_relaunch()`` pins that store, so the relaunched
        process runs the same silent challenge a returning user does.

        Distinct from ``preserve_state_across_relaunch()``, which asks whether the
        STORE survives; this asks whether the IDENTITY survives into it. **No
        driver answers False today** — web was overridden False for one day
        (2026-08-05) on a premise that measurement disproved: its agent writes the
        injected session to ``localStorage`` and the reloaded SPA re-hydrates from
        it, so web's identity survives exactly like the native ones. The predicate
        is kept because the two questions really are distinct and a future driver
        may separate them; it is NOT kept as a record of a web gap.

        Used to gate assertions that need an identity to outlive a process
        boundary. NOT the same question as ``hard_reload()``, which replays the
        remembered session on the native path — deliberately not on web, where ~30
        tests (``test_sign_out_web`` first among them) depend on a reload landing
        signed OUT (there the SPA's own sign-out has cleared ``fauna_secret``, so
        the reload has nothing to re-hydrate — no replay suppression needed)."""
        return True

    def supports_unclean_kill(self) -> bool:
        """True when the app under test is a Popen this driver spawned itself
        (linux/macOS set ``self._app_proc``). web/windows subclass this driver too
        but don't own an app child — they stay False. **tui owns its child as a pty
        session leader, not a Popen**, so it overrides this rather than being
        covered here — the earlier version of this docstring claimed tui set
        ``_app_proc``, which it never has, and that claim is what made 11
        crash-recovery skips look structural."""
        return getattr(self, "_app_proc", None) is not None

    def preserve_state_across_relaunch(self) -> bool:
        """Ask the driver to keep the app's client-local store across the next
        relaunch, and report whether it can.

        The default relaunch contract is the opposite (see ``hard_reload``): a
        relaunched native app comes back with a fresh store and the driver replays
        the session, so "anything a force-quit + relaunch must outlive has to live
        nest-side". A test asserting *client-side* durability — the pending-factory-
        reset slot of gap CR-1 (``common.md`` § Client-state recoverability) — needs
        the opposite, and calls this first.

        False here means the driver cannot pin its store, so such a test must skip
        rather than pass vacuously (a fresh store makes "the slot did not survive"
        indistinguishable from "the client never wrote it"). Overridden by linux;
        web needs no override (a page reload keeps the same origin's localStorage).
        """
        return False

    #: The `_launch_config` keys `preserve_state_across_relaunch()` pins (linux +
    #: tui set all three; any bridge driver that adds a store pin uses this
    #: vocabulary). `_clear_relaunch_pin()` is their un-pin counterpart.
    _RELAUNCH_PIN_KEYS = ("xdg_base", "credential_dir", "keyring_app")

    def _record_relaunch_pin(self, config: dict, keys) -> None:
        """Remember that *we* pinned `keys` into `config`, so `_clear_relaunch_pin`
        can undo exactly those and never a caller-supplied value. Call from a
        `preserve_state_across_relaunch()` override right after writing the keys."""
        self._preserve_pinned_keys = set(keys)

    def _clear_relaunch_pin(self) -> None:
        """Undo any `preserve_state_across_relaunch()` pin so the NEXT relaunch
        (`recover()` / `hard_reload()`) gets a fresh client-local store — unless
        the running test re-pins after this.

        Called at the top of `reset()` (the per-test boundary), this is the native
        analogue of macOS clearing `_preserved_cred_dir` in its own reset(). Without
        it the session-scoped driver's `_launch_config` keeps a prior factory-reset
        journey's pinned dirs and leaks that store into every later test's relaunch.

        Only un-pins keys THIS driver's preserve() actually added. The old blanket
        pop of `_RELAUNCH_PIN_KEYS` rested on "the three keys are only ever set by an
        explicit preserve() call in a test body, never at fixture setup" — which is
        false: `common.cred_store`'s launch_config supplies `xdg_base`/`keyring_app`
        at setup, and `LaunchHarness` hands the driver that very dict (it aliases
        `_launch_config` on purpose, so a later preserve() reaches the config
        relaunch() re-uses). So reset() silently deleted the harness's own pinning
        and the next relaunch died on a missing key — `test_smoke_g` was RED on main
        with `KeyError: 'xdg_base'`. A test that never calls preserve() has nothing
        to un-pin."""
        config = getattr(self, "_launch_config", None)
        pinned = getattr(self, "_preserve_pinned_keys", None)
        if isinstance(config, dict) and pinned:
            for key in pinned:
                config.pop(key, None)
        self._preserve_pinned_keys = set()

    # ── The principal-slot carry (e2e-conventions.md convention 10) ─────────
    #
    # A relaunch is the same MACHINE restarting its app, not a new machine. A
    # real install keeps its per-actor store-principal slot — the
    # `fauna-account-store` writer key and the principal bundle beside it —
    # across a restart; a launch that loses it mints a fresh writer key — under
    # the retired two-row shape a fresh placeholder device row per relaunch,
    # until the session actor met its device cap; under one credential, one row
    # (`sync-agent-credentials.md` § Credential model, RULED 2026-09-28), a fresh
    # principal re-enrolled on the machine's named row, which
    # `tests/test_relaunch_device_accrual.py`'s writer-key assertion catches.
    # **And the slot never travels
    # without its replica** (2026-09-15): a writer lives exactly as long as its
    # journal (`account-replica-posture.md` § The store device principal,
    # refinement 11), so a key restored over a fresh account-store dir is a key
    # the app abandons on the spot — it mints a fresh writer and enrolls a new
    # device, the very accrual the carry exists to stop — and before that ruling
    # the same shape re-issued seqs the nest already held (`stale_writer_seq`,
    # 84 refusals in one linux sweep). So the carry harvests the actor's
    # `account-store` dir beside its slot and restores the two together: a
    # relaunch models a true restart of the machine's store principal.
    # A driver whose `launch()` gives every launch a fresh file-backed
    # credential store (linux, tui, and macOS + iOS in
    # `InProcessAgentDriver._resolve_credential_store`) calls
    # `_begin_principal_slot_carry` there; the only other thing that crosses
    # their relaunch is the install device secret (below: linux and tui through
    # `_carry_install_device_secret`, macOS and iOS through its row-shaped twin
    # `_carry_install_device_secret_row`) — the identity trio and the rest of the
    # per-launch app data stay fresh. windows reaches
    # the same place by a different route, because its relaunch re-posts the same
    # session body and so cannot be handed a new dir: `recover()` empties the one
    # store the driver keeps (`_erase_credential_store_for_relaunch`), which is
    # why it calls `_begin_principal_slot_carry` in `launch()` for the decision
    # and `_relaunch_principal_slot_carry` in `recover()`, right before that
    # erase. Until 2026-09-21 it emptied nothing and let the `reset()` after the
    # relaunch take the slot instead — a live account runtime reached that
    # sign-out-shaped stop, so the relaunch retired the very enrollment this
    # carry then restored. Either
    # way `set_state` restores the slot at the actor's first sign-in after the
    # relaunch, into the file `_principal_slot_store` names — on iOS the app's
    # own `keychain.json`, which FaunaKit's e2e backing re-reads on every
    # operation for exactly this — and the replica into that launch's store
    # root, which every driver publishes as `_resolved_store_root` AFTER the
    # carry begins (so the harvest reads the launch being replaced).

    def _begin_principal_slot_carry(self, config: dict, env: dict) -> None:
        """Start a launch's carry. Call from `launch()` after building the child
        env and BEFORE the `_resolved_*` store attrs move to the new launch: it
        harvests the launch being replaced, then decides whether the new one may
        be restored into.

        Never into a store the caller owns — a supplied `credential_dir` or
        `keyring_app` (including a `preserve_state_across_relaunch()` pin, whose
        store already holds its slot), `use_real_keyring`, or an empty
        `FAUNA_E2E_CREDENTIAL_DIR` (tui's sealed `headless_store`)."""
        self._relaunch_principal_slot_carry()
        self._principal_slot_carry_live = bool(
            env.get("FAUNA_E2E_CREDENTIAL_DIR")
            and not config.get("credential_dir")
            and not config.get("keyring_app")
            and not config.get("use_real_keyring")
        )

    def _relaunch_principal_slot_carry(self) -> None:
        """Restart the carry for one relaunch, keeping the launch's decision about
        whether its store may be restored into: harvest the store as the process
        being replaced left it, and re-arm the once-per-launch restore.

        `_begin_principal_slot_carry` runs it on every `launch()`. A driver whose
        relaunch does not go through `launch()` (windows' `recover()`) calls it
        itself, once per relaunch, after the old process is gone and before the
        new one starts — never per `reset()`, which would restore across a
        sign-out → sign-in inside one process."""
        self._harvest_principal_slots(
            getattr(self, "_resolved_credential_dir", None),
            getattr(self, "_resolved_keyring_app", None),
        )
        self._principal_slots_restored = set()

    def _principal_slot_store(self, cred_dir, keyring_app):
        """Where one launch's `fauna-account-store` entries rest, as `(file, key
        prefix)` — or `None` for a launch that resolved no file-backed store.

        Read from the harness's one table (`common.cred_store.account_store_location`)
        rather than spelled here: the namespace is a file of its own on linux, tui,
        windows and macOS, but on iOS it rides the foreign seam into the app's own
        `keychain.json`, each row prefixed, beside the identity rows."""
        if not cred_dir or not keyring_app:
            return None
        from common.cred_store import account_store_location
        from helpers.app_surface import app_name

        return account_store_location(app_name(self), cred_dir, keyring_app)

    def _harvest_principal_slots(self, cred_dir, keyring_app) -> None:
        """Fold one launch's per-actor slots into the driver's vault.

        Whole slots only: an actor's slot is its bare writer-key entry plus every
        `<actor>/…` entry beside it, and entries with no writer key are not a
        machine's principal. Per actor the newest launch that held it wins (a
        re-minted key replaces the older one); an actor this launch never held
        keeps what an earlier launch left.

        An actor this launch SIGNED IN and no longer holds was signed OUT in it,
        and its vault copy is dropped: the sign-out retired that enrollment
        nest-side and erased the slot, which is where a real machine stands after
        a sign-out and a restart — holding no key for the account. Restoring the
        earlier launch's copy would hand the next launch a credential the nest
        has already forgotten (principal succession mints a successor on a fresh
        row, and the revoked-key handshake's retries stretch that sign-in long
        enough for the next reset's stop to lapse and strand the row)."""
        store = self._principal_slot_store(cred_dir, keyring_app)
        if store is None:
            return
        signed_in = set(getattr(self, "_principal_slots_restored", ()))
        path, prefix = store
        try:
            with open(path) as f:
                stored = json.load(f)
        except FileNotFoundError:
            # The erase can take the whole namespace file: a store holding nothing.
            stored = {}
        except (OSError, ValueError):
            return
        if not isinstance(stored, dict):
            return
        # The namespace's own rows by their account names — on iOS the file is the
        # app's whole keychain, and only the prefixed rows are the account store's.
        stored = {k[len(prefix):]: v for k, v in stored.items() if k.startswith(prefix)}
        vault = self.__dict__.setdefault("_principal_slot_vault", {})
        for actor in stored:
            if len(actor) == 64 and all(c in "0123456789abcdef" for c in actor):
                prefix = f"{actor}/"
                vault[actor] = {
                    k: v for k, v in stored.items() if k == actor or k.startswith(prefix)
                }
                self._harvest_replica(actor)
        for actor in signed_in - set(stored):
            vault.pop(actor, None)
            self.__dict__.get("_replica_vault", {}).pop(actor, None)

    @staticmethod
    def _replica_dir(store_root, actor) -> str | None:
        """Where `actor`'s account store rests under a launch's unified store
        root: `<root>/<actor-id-hex>/account-store`, the one layout shared Rust
        resolves (`fauna_account_store::root::StoreRoot::store_dir`). The rest of
        the actor dir (per-set engine state) is deliberately NOT part of the
        carry: only the journal must live and die with the writer key."""
        if not store_root or not actor:
            return None
        return os.path.join(store_root, actor, "account-store")

    def _harvest_replica(self, actor: str) -> None:
        """Fold one launch's replica for `actor` into the driver's vault, a COPY
        taken while the launch being replaced still has it — the reset() after
        a relaunch erases the source. The newest launch that held it wins, as
        for the slot. Called only for an actor whose whole slot was harvested:
        a replica without its key is not a machine's principal, and the app
        would refuse or re-adopt it on its own terms."""
        src = self._replica_dir(getattr(self, "_resolved_store_root", None), actor)
        if not src or not os.path.isdir(src):
            return
        vault_root = self.__dict__.get("_replica_vault_dir")
        if vault_root is None:
            vault_root = tempfile.mkdtemp(prefix="fauna-e2e-replica-vault-")
            self._replica_vault_dir = vault_root
        dst = os.path.join(vault_root, actor)
        shutil.rmtree(dst, ignore_errors=True)
        shutil.copytree(src, dst)
        self.__dict__.setdefault("_replica_vault", {})[actor] = dst

    def _restore_replica(self, actor: str) -> None:
        """Lay `actor`'s harvested replica down under THIS launch's store root,
        beside the slot just restored — never over one the launch already
        holds. Called from the slot restore only, on the branch that wrote the
        slot: the replica travels with its key or not at all."""
        src = getattr(self, "_replica_vault", {}).get(actor)
        dst = self._replica_dir(getattr(self, "_resolved_store_root", None), actor)
        if not src or not dst or os.path.exists(dst):
            return
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        shutil.copytree(src, dst)

    def _restore_principal_slot(self, session: dict) -> None:
        """Before a session patch signs an actor in, restore that actor's
        harvested slot into this launch's store.

        Once per launch per actor: a sign-out → sign-in inside one process must
        mint afresh, exactly as production does, or an erase that dropped the
        slot but kept the store would be masked. Only the actor being signed in:
        the reset erase reaches only actors the running process's registry knows,
        so any other actor's slot copied in would be residue no sign-out can
        remove. Never over a slot the store already holds, and never disturbing
        anything else the file holds (on iOS, the app's own identity rows). The
        write takes the file's `<file>.lock` — the Rust File backend's lock, which
        FaunaKit's e2e keychain backing takes too — and lands tmp+rename at 0600,
        as `fauna_credential_store::cred_file_write` does."""
        if not session.get("authenticated") or not getattr(
            self, "_principal_slot_carry_live", False
        ):
            return
        actor = session.get("actor_id")
        if not isinstance(actor, str) or not actor:
            return
        actor = actor.lower()
        restored = self.__dict__.setdefault("_principal_slots_restored", set())
        if actor in restored:
            return
        restored.add(actor)
        slot = getattr(self, "_principal_slot_vault", {}).get(actor)
        store = self._principal_slot_store(
            getattr(self, "_resolved_credential_dir", None),
            getattr(self, "_resolved_keyring_app", None),
        )
        if not slot or store is None:
            return
        path, prefix = store
        if not self._merge_store_rows(
            path,
            {f"{prefix}{k}": v for k, v in slot.items()},
            skip_if_present=f"{prefix}{actor}",
        ):
            return
        # The replica travels with its key (refinement 11): a slot restored
        # over a fresh account-store dir is a key the app abandons on sight.
        self._restore_replica(actor)

    def _merge_store_rows(self, path: str, rows: dict, *, skip_if_present: str) -> bool:
        """Merge `rows` into the JSON credential store at `path`, leaving every
        row already beside them untouched; skip the whole write — returning
        `False` — when the store already holds `skip_if_present`.

        The carry's ONE spelling of its write, shared by the principal slot and
        the install-secret row below: it takes the file's `<file>.lock` — the
        Rust File backend's lock, which FaunaKit's e2e keychain backing takes too
        — and lands tmp+rename at 0600, as
        `fauna_credential_store::cred_file_write` does. A second copy of it would
        be a second place for the lock, the mode or the merge to drift, on a file
        the app reads as key material."""
        try:
            import fcntl
        except ImportError:  # windows: unserialized, the backend's own degrade
            fcntl = None
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        with open(f"{path}.lock", "a") as lock:
            if fcntl is not None:
                fcntl.flock(lock, fcntl.LOCK_EX)
            try:
                with open(path) as f:
                    current = json.load(f)
            except (OSError, ValueError):
                current = {}
            if not isinstance(current, dict):
                current = {}
            if skip_if_present in current:
                return False
            current.update(rows)
            tmp = f"{path}.{os.getpid()}.carry.tmp"
            fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            with os.fdopen(fd, "w") as f:
                json.dump(current, f, indent=2, sort_keys=True)
                f.flush()
                os.fsync(f.fileno())
            os.replace(tmp, path)
        return True

    # ── The install-device-secret carry (convention 10, ruled 2026-09-20) ───
    #
    # The one other thing a relaunch keeps. The id of the named sync device row
    # an account registers is derived from an install-scoped secret and the
    # account (`sync-agent-credentials.md` § Credential model, the RULED
    # 2026-09-20 block), so a launch whose data dir is fresh is a new machine to
    # the nest: one new named row per relaunch for every actor that signs in
    # with an id the app refuses to adopt, and a sweep's session actor walks
    # toward its device cap exactly as it did before the principal carry. Unlike
    # the principal slot the secret names no account, so it is laid down AT
    # launch, before the app starts, never at a sign-in. Where the secret rests
    # is per app: linux and tui keep it as a file under the install dir their
    # driver passes here, macOS and iOS as a row in the app's own `keychain.json`
    # (`_carry_install_device_secret_row`), and each remaining app's leg, once the
    # app derives its id from the secret, extends this one carry to wherever that
    # app keeps it rather than growing a second one.

    #: Shared Rust's name for the secret's file under an app's install-scoped
    #: sync dir (`fauna_sync_engine::engine_lifecycle::INSTALL_DEVICE_SECRET_FILE`)
    #: and its length (`fauna_core::device_id::INSTALL_DEVICE_SECRET_LEN`).
    INSTALL_DEVICE_SECRET_FILE = "install-device-secret"
    INSTALL_DEVICE_SECRET_LEN = 32

    def _carry_install_device_secret(self, install_dir: str | None) -> None:
        """Carry the install device secret into the launch whose install dir is
        `install_dir`. Call from `launch()` AFTER `_begin_principal_slot_carry`
        (whose decision it reuses) and BEFORE the app starts.

        The harvest reads what the launch being replaced left on disk — a
        restart keeps the disk, not an older launch's copy — so a launch that
        lost its secret (its data dir wiped: the fresh-machine case) carries
        none forward. Only a secret of exactly `INSTALL_DEVICE_SECRET_LEN` bytes
        travels: anything else is a torn mint (a crash between the mint's
        `create_new` and its write leaves 0 bytes) that every later derivation
        refuses for good, so it is dropped and the new launch mints afresh.

        The fresh-machine opt-out is the principal carry's: never into a launch
        whose store the caller owns (`_begin_principal_slot_carry`'s docstring),
        and never over a secret the launch already holds. Written tmp+rename at
        0600, the mode shared Rust mints it with."""
        previous = getattr(self, "_resolved_install_dir", None)
        if previous:
            try:
                with open(os.path.join(previous, self.INSTALL_DEVICE_SECRET_FILE), "rb") as f:
                    secret = f.read()
            except OSError:
                secret = None
            if secret is not None and len(secret) != self.INSTALL_DEVICE_SECRET_LEN:
                secret = None
            self._install_device_secret = secret
        self._resolved_install_dir = install_dir
        secret = getattr(self, "_install_device_secret", None)
        if not install_dir or secret is None or not getattr(
            self, "_principal_slot_carry_live", False
        ):
            return
        path = os.path.join(install_dir, self.INSTALL_DEVICE_SECRET_FILE)
        if os.path.exists(path):
            return
        os.makedirs(install_dir, exist_ok=True)
        tmp = f"{path}.{os.getpid()}.carry.tmp"
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "wb") as f:
            f.write(secret)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)

    #: The logical key a SECRET-STORE app keeps the same secret under
    #: (`fauna_client_accounts::device_id::INSTALL_DEVICE_SECRET`), its value
    #: `INSTALL_DEVICE_SECRET_LEN` bytes as lowercase hex. apple's
    #: `KeychainSecretStore` maps every logical key verbatim, so this is also
    #: the row's name inside `keychain.json`.
    INSTALL_DEVICE_SECRET_KEY = "install/device_secret"

    def _begin_install_device_secret_row_carry(self, store_file: str | None) -> None:
        """Start a launch's carry for an app that keeps the secret as a ROW in its
        credential store instead of a file under an install dir
        (:meth:`_carry_install_device_secret`'s twin).

        The same carry, the same bounds — only the at-rest shape differs, which
        is exactly what convention 10 means by "each other app's leg extends this
        one carry to wherever that app keeps its secret". apple has no install
        dir to copy a file into: its secret is a row in the app's own
        `keychain.json`, written by the shared registry through
        `KeychainSecretStore` under :data:`INSTALL_DEVICE_SECRET_KEY`.

        Call it LAST in the store half of `launch()` — after
        `_begin_principal_slot_carry`, whose live/owned decision it reuses. It
        harvests the launch being replaced, publishes this launch's store, and
        arms the once-per-launch restore below.

        The harvest reads the store as the launch being replaced left it, so a
        launch whose store was wiped carries none forward (the fresh-machine
        case), and only a value decoding to exactly `INSTALL_DEVICE_SECRET_LEN`
        bytes travels, since a torn mint is one every later derivation refuses
        for good."""
        previous = getattr(self, "_resolved_install_secret_store", None)
        if previous:
            self._install_device_secret_row = self._read_install_device_secret_row(previous)
        self._resolved_install_secret_store = store_file
        self._install_device_secret_row_restored = False

    def _restore_install_device_secret_row(self, session: dict) -> None:
        """Lay the harvested secret into THIS launch's store, before the app sees
        the sign-in — the same seam and the same moment as
        :meth:`_restore_principal_slot`, which is what makes the carry survive.

        **Why a row cannot be laid down at launch the way a file is.** The file
        leg writes before the app starts and nothing touches it again; on a
        secret-store app the store IS the app's keychain, and every apple
        relaunch is followed by a `reset()` whose `resetToFactory` sweeps *every*
        row in the service (`KeychainStore.deleteAll`, deliberately not the
        identity three). A row written at launch is therefore erased before the
        first sign-in can derive anything from it — the same shape that made
        windows' principal slot harvest in `recover()` rather than at launch.
        Restoring at the sign-in puts the secret back on the far side of that
        erase.

        Once per LAUNCH, not per actor: the secret names no account, so no
        other actor's residue can ride in with it, and a *later* reset inside one
        launch is a factory reset, which the ruling says ends the install secret
        — so the next sign-in after it correctly mints afresh. Never into a store
        the caller owns (`_begin_principal_slot_carry`'s bound), and never over a
        secret the launch already holds."""
        store_file = getattr(self, "_resolved_install_secret_store", None)
        secret = getattr(self, "_install_device_secret_row", None)
        if (
            not session.get("authenticated")
            or not store_file
            or secret is None
            or getattr(self, "_install_device_secret_row_restored", False)
            or not getattr(self, "_principal_slot_carry_live", False)
        ):
            return
        self._install_device_secret_row_restored = True
        self._merge_store_rows(
            store_file,
            {self.INSTALL_DEVICE_SECRET_KEY: secret},
            skip_if_present=self.INSTALL_DEVICE_SECRET_KEY,
        )

    def _read_install_device_secret_row(self, store_file: str) -> str | None:
        """The install device secret a store holds, as the app spells it — or
        `None` for an absent store, a missing row, or a value that is not
        `INSTALL_DEVICE_SECRET_LEN` bytes of hex.

        The hex is decoded only to CHECK it: what travels is the app's own
        string, so the carry never re-spells a row shared Rust will read back
        (`fauna_core::hex32::decode` is what refuses a torn one there)."""
        try:
            with open(store_file) as f:
                stored = json.load(f)
        except (OSError, ValueError):
            return None
        value = stored.get(self.INSTALL_DEVICE_SECRET_KEY) if isinstance(stored, dict) else None
        if not isinstance(value, str):
            return None
        try:
            raw = bytes.fromhex(value)
        except ValueError:
            return None
        return value if len(raw) == self.INSTALL_DEVICE_SECRET_LEN else None

    def kill_uncleanly(self) -> None:
        """SIGKILL our OWN app child (see PlatformDriver.kill_uncleanly).

        Kills the process GROUP (the child was spawned as a group leader via
        ``port_util.popen_group_kwargs``) so any helper processes die with it —
        power-loss semantics, and no orphans. SIGKILL is uncatchable: no
        SIGTERM handlers, no atexit, no flush — the genuine crash.

        The dead Popen stays in ``self._app_proc`` so the next ``teardown()``
        (via ``hard_reload()`` → ``recover()``) closes log files and untracks
        it; teardown already tolerates an exited child.
        """
        proc = getattr(self, "_app_proc", None)
        if proc is None:
            super().kill_uncleanly()  # the NotImplementedError with the contract
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        except (ProcessLookupError, PermissionError, OSError):
            proc.kill()
        proc.wait(timeout=10)

    #: Ceiling for a PINNED-store relaunch's own auto-login to land (a named
    #: generous budget + deadline poll, convention 14 — never an expectation:
    #: measured landings on a localhost nest are ~1.5 s). Only consulted when
    #: ``preserve_state_across_relaunch()`` pinned the store; past it,
    #: ``hard_reload`` falls back to the unpinned contract's login replay.
    AUTO_RELOGIN_CEILING_S = 30.0

    def hard_reload(self) -> None:
        """Force-quit + relaunch the app, preserving the injected identity — the
        native-client equivalent of the web driver's full page reload (which
        overrides this with ``location.reload``).

        Two contracts, split on the store pin:

        **Unpinned (default):** a relaunched native process starts
        unauthenticated (its identity lived in process memory / an in-memory
        keychain, not in the data dir we keep). We relaunch via ``recover()`` —
        each native subclass (windows/linux/macOS/iOS) overrides it to teardown
        + launch its own process — then replay the last ``set_state``
        ``session`` block, the exact mechanism that logged the app in initially
        (conftest ``logged_in_app``), so the app re-authenticates as the SAME
        actor and its launch-time restore (e.g. conversations
        ``restoreDrafts``) re-fetches nest-persisted state. Anything a
        "force-quit + relaunch" must outlive therefore has to live nest-side
        (the ``__drafts`` plane), exactly as on web: the reload drops in-memory
        state but keeps the identity (there, ``localStorage``; here, the
        replayed session) so the same actor reconnects.

        **Pinned (``preserve_state_across_relaunch()`` was called):** the
        relaunched app auto-restores its OWN session from the preserved store —
        a real user's restart, where nobody re-logs-in. Replaying the login on
        top of that raced the auto-login into a SECOND concurrent same-actor
        login, and the loser's conversations-engine build hit the winner's
        role lock (``StateServedElsewhere``) — a standing refusal that bricked
        the rail on tui and blanked the conversations page on linux (measured). So here we WAIT
        for the app's own auto-login instead (deadline poll under
        ``AUTO_RELOGIN_CEILING_S``), and only fall back to the replay if it
        never lands — the unpinned contract, for a store whose credentials
        genuinely did not survive.
        """
        if not self.recover():
            raise RuntimeError(
                "hard_reload could not relaunch the app "
                "(no launch config, or the relaunch failed)"
            )
        session = getattr(self, "_last_session", None)
        if session is None:
            return
        if getattr(self, "_preserve_pinned_keys", None):
            want = session.get("actor_id")
            deadline = time.monotonic() + self.AUTO_RELOGIN_CEILING_S
            while time.monotonic() < deadline:
                try:
                    if self.get_state("session.authenticated") and (
                        want is None or self.get_state("session.actor_id") == want
                    ):
                        return
                except (LookupError, RuntimeError, OSError):
                    pass  # a still-booting bridge answers when it answers
                time.sleep(0.5)
            # No auto-login inside the ceiling: the pinned store did not carry
            # the credentials after all — fall through to the replay.
        # Re-establish the authenticated session and land on the feed, mirroring
        # logged_in_app's initial injection so the app reconnects + restores.
        self.set_state({"session": session, "nav": {"stack": [{"view": "feed"}]}})

    def _post(self, path: str, data: dict | None = None,
              timeout: float | None = None) -> dict:
        # ``timeout`` defaults to the shared generous ceiling (resolved at call
        # time — see BRIDGE_RPC_TIMEOUT_S for why it is one constant, not
        # per-call-site bumps). Known-heaviest calls (web's browser launch /
        # SPA reboot) still pass an even larger explicit value
        # (web.py HEAVY_BOOT_TIMEOUT_S).
        return self._post_raw(path, json.dumps(data or {}).encode(),
                              "application/json", timeout=timeout)

    def _post_raw(self, path: str, body: bytes, content_type: str,
                  timeout: float | None = None) -> dict:
        """``_post`` for a body that is not a JSON object — the bytes verbatim.

        Same liveness, timeout and error classification as every other bridge
        call; only the body and its ``Content-Type`` differ. ``path`` carries any
        query string itself. Used where the bridge must receive opaque bytes
        (android's ``set_input_files``), which a JSON string would mangle or
        inflate."""
        if timeout is None:
            timeout = BRIDGE_RPC_TIMEOUT_S
        self._check_bridge()
        req = urllib.request.Request(
            f"{self._url}{path}",
            data=body,
            headers={"Content-Type": content_type},
            method="POST",
        )
        try:
            resp = urllib.request.urlopen(req, timeout=timeout)
            self._bridge_answered()
            return json.loads(resp.read())
        except urllib.error.HTTPError as e:
            self._bridge_answered()
            error_body = e.read().decode()
            try:
                msg = json.loads(error_body).get("error", error_body)
            except json.JSONDecodeError:
                msg = error_body
            if e.code == 404:
                raise LookupError(msg)
            raise RuntimeError(
                f"Bridge error ({e.code}): {msg}{self._bridge_error_note(msg)}")
        except OSError as e:
            # Covers refused/reset (raw or URLError-wrapped) AND socket
            # timeouts — but the two mean opposite things: refused/reset is
            # the peer signalling "nothing here" (immediate death), a timeout
            # is a slow-but-accepting server (candidate for life). Classify
            # before concluding anything (_timeout_verdict never returns).
            if self._is_timeout(e):
                self._timeout_verdict("POST", path, e, timeout)
            self._mark_dead("POST", path, e)
            raise BridgeDead(f"Bridge died: {e}") from e

    def _scope_wire(self, scope: str | None) -> list[dict] | None:
        if not scope:
            return None
        return scope_to_wire(parse_scope(scope))

    def _get(self, path: str, params: dict | None = None) -> dict:
        body = self._get_bytes(path, params)
        if not body:
            return {}
        return json.loads(body)

    def _get_bytes(self, path: str, params: dict | None = None) -> bytes:
        """``_get`` for an answer that is not a JSON object — the body verbatim
        (empty for a 204). Same liveness, timeout and error classification as
        every other bridge call. Used where the bridge hands back opaque bytes
        (android's ``download_dir`` mirror), the read twin of ``_post_raw``."""
        self._check_bridge()
        timeout = BRIDGE_RPC_TIMEOUT_S
        url = f"{self._url}{path}"
        if params:
            url += f"?{urllib.parse.urlencode(params)}"
        try:
            resp = urllib.request.urlopen(url, timeout=timeout)
            self._bridge_answered()
            if resp.status == 204:
                return b""
            return resp.read()
        except urllib.error.HTTPError as e:
            # HTTP errors are bridge-returned errors, not bridge death.
            self._bridge_answered()
            error_body = e.read().decode()
            try:
                msg = json.loads(error_body).get("error", error_body)
            except json.JSONDecodeError:
                msg = error_body
            if e.code == 404:
                raise LookupError(msg)
            raise RuntimeError(
                f"Bridge error ({e.code}): {msg}{self._bridge_error_note(msg)}")
        except OSError as e:
            # Same classification as _post: timeout = candidate for life,
            # refused/reset = immediate death (_timeout_verdict never returns).
            if self._is_timeout(e):
                self._timeout_verdict("GET", path, e, timeout)
            self._mark_dead("GET", path, e)
            raise BridgeDead(f"Bridge died: {e}") from e

    def _delete(self, path: str) -> dict:
        # Deliberately NOT BRIDGE_RPC_TIMEOUT_S: _delete is teardown-only and
        # swallows every error, so a generous wait here buys nothing — the
        # process gets terminated right after regardless.
        req = urllib.request.Request(f"{self._url}{path}", method="DELETE")
        try:
            resp = urllib.request.urlopen(req, timeout=30)
            return json.loads(resp.read())
        except Exception:
            return {}

    # --- Scroll support ---

    _MAX_SCROLL_RETRIES = 3

    # Bridges that implement POST /element/scroll-into-view (a *targeted* scroll
    # that brings one element into its scroll container's viewport) opt in by
    # setting this True. Blind page-step scroll() overshoots mid-page elements
    # (a LargeIncrement can jump an offscreen element from below the fold to
    # above it without ever landing it visible). Default False so non-supporting
    # bridges never make the failing round-trip.
    _supports_scroll_into_view = False

    def _scroll_into_view(self, element_id: str, index: int = 0, *,
                          scope: str | None = None) -> bool:
        """Best-effort targeted scroll-into-view. No-op (returns False) on
        bridges that don't support it or when the element isn't in the tree."""
        if not self._supports_scroll_into_view:
            return False
        body: dict = {"id": element_id, "index": index}
        wire = self._scope_wire(scope)
        if wire:
            body["scope"] = wire
        try:
            return bool(self._post("/element/scroll-into-view", body).get("found", False))
        except Exception:
            return False

    def scroll_to(self, element_id: str, index: int = 0, *, scope: str | None = None) -> None:
        """Targeted scroll-into-view that FAILS LOUDLY: raises on a bridge
        without POST /element/scroll-into-view support and on an absent
        element, so a test dwell can never silently measure nothing."""
        if not self._supports_scroll_into_view:
            raise NotImplementedError(
                f"{type(self).__name__}'s bridge does not serve /element/scroll-into-view"
            )
        body: dict = {"id": element_id, "index": index}
        wire = self._scope_wire(scope)
        if wire:
            body["scope"] = wire
        resp = self._post("/element/scroll-into-view", body)
        if not resp.get("found", False):
            raise LookupError(
                f"scroll_to({element_id!r}[{index}]): {resp.get('error', 'not found')}"
            )
        # Bridges that report the post-scroll geometry (apple) stash it for
        # `last_scroll_geometry()`; others leave it None and callers degrade to
        # a plain visibility read.
        self._last_scroll_geometry = (
            (resp.get("frame"), resp.get("viewport"))
            if resp.get("frame") and resp.get("viewport") else None
        )

    #: Post-scroll geometry of the most recent `scroll_to`, when the bridge
    #: reports it (apple's in-process agent does).
    _last_scroll_geometry: tuple[str, str] | None = None

    def last_scroll_geometry(self) -> tuple[tuple[float, float, float, float],
                                            tuple[float, float]] | None:
        """`((x, y, w, h), (viewport_w, viewport_h))` for the last `scroll_to`,
        or None on a bridge that doesn't report it.

        Lets a test assert how much of the element the scroll ACTUALLY put on
        screen. `found: true` only says a scroll happened — the gap between
        "scrolled" and "scrolled far enough to be a real exposure" is precisely
        where an edge-aligned scroll would fake a dwell (windows' FlaUI flips
        `!IsOffscreen` at ~25% visibility, under the shared 500‰/750‰ cue gates).
        """
        if not self._last_scroll_geometry:
            return None
        frame_s, viewport_s = self._last_scroll_geometry
        x, y, w, h = (float(p) for p in frame_s.split(","))
        vw, vh = (float(p) for p in viewport_s.split(","))
        return (x, y, w, h), (vw, vh)

    def scroll(self, direction: str = "down") -> None:
        """Send a scroll/swipe gesture. All native bridges implement POST /scroll."""
        try:
            self._post("/scroll", {"direction": direction})
        except Exception:
            pass  # Best-effort — web bridge doesn't need it

    def _post_with_scroll(self, path: str, body: dict) -> dict:
        """POST to a bridge element endpoint, retrying with scroll on 404.

        SwiftUI Lists, WinUI ScrollViewers, and GTK ListBoxes lazily render
        cells.  Offscreen elements may not exist in the accessibility tree.
        When the bridge returns 404 (element not found), scroll down and retry
        up to _MAX_SCROLL_RETRIES times to load the element into view.
        """
        try:
            return self._post(path, body)
        except LookupError:
            for _ in range(self._MAX_SCROLL_RETRIES):
                self.scroll("down")
                try:
                    return self._post(path, body)
                except LookupError:
                    continue
            raise  # re-raise the last LookupError

    # --- PlatformDriver interface ---

    def launch(self, config: dict) -> None:
        raise NotImplementedError("Subclasses must implement launch()")

    def teardown(self) -> None:
        raise NotImplementedError("Subclasses must implement teardown()")

    def find_element(self, element_id: str, index: int = 0, *, scope: str | None = None):
        return _ElementStub(self, element_id, index, scope)

    def click(self, element_id: str, index: int = 0, *, scope: str | None = None) -> None:
        body: dict = {"id": element_id, "index": index}
        wire = self._scope_wire(scope)
        if wire:
            body["scope"] = wire
        self._post_with_scroll("/element/click", body)

    def double_click(self, element_id: str, index: int = 0, *, scope: str | None = None) -> None:
        body: dict = {"id": element_id, "index": index}
        wire = self._scope_wire(scope)
        if wire:
            body["scope"] = wire
        self._post_with_scroll("/element/double_click", body)

    def type_text(self, element_id: str, text: str, *, scope: str | None = None) -> None:
        body: dict = {"id": element_id, "text": text}
        wire = self._scope_wire(scope)
        if wire:
            body["scope"] = wire
        self._post_with_scroll("/element/type", body)

    def clear_and_type(self, element_id: str, text: str, *, scope: str | None = None) -> None:
        body_clear: dict = {"id": element_id}
        body_type: dict = {"id": element_id, "text": text}
        wire = self._scope_wire(scope)
        if wire:
            body_clear["scope"] = wire
            body_type["scope"] = wire
        self._post_with_scroll("/element/clear", body_clear)
        # Clearing can trigger a re-layout (validation text appearing/
        # disappearing, a virtualizing ScrollViewer discarding the now-offscreen
        # element) that invalidates the "still in view" assumption a plain
        # _post relied on — retry-with-scroll here too (found via a live
        # windows sweep: admin-mail's IMAP/alias fields, both lower on the
        # page, 404'd on type immediately after a successful clear).
        self._post_with_scroll("/element/type", body_type)

    def select(self, element_id: str, value: str, *, index: int = 0,
               scope: str | None = None) -> None:
        """Choose `value` in the picker `element_id`.

        Refused when the app never painted `value` as an option — see
        `SelectOptionNotOffered`. Every app refuses; only *how* differs, because
        the four that drive a real widget (linux's `StringObject` model lookup,
        web's Playwright `select_option`, windows' `ComboBoxItem` scan, android's
        `By.text`) cannot actuate an option they can't find, while a
        value-writeback registry (tui, apple) has to check membership explicitly
        and answers 409.
        """
        body: dict = {"id": element_id, "value": value, "index": index}
        wire = self._scope_wire(scope)
        if wire:
            body["scope"] = wire
        try:
            self._post_with_scroll("/element/select", body)
        except RuntimeError as e:
            # Two DIFFERENT refusals answer 409 on this route, and only the
            # message tells them apart: the picker was disabled (convention 11's
            # actuation gate — `gate_actuation`, which fires BEFORE the
            # membership check precisely so a disabled picker with an empty
            # option list does not report the wrong one), or the value was not
            # among the painted options. Wrapping the first as
            # `SelectOptionNotOffered` would put the wrong contract in the
            # traceback and send the reader hunting an option list that was
            # never the problem, so it propagates as the app's own self-
            # diagnosing message.
            if "(409)" in str(e) and _DISABLED_ACTUATION_REFUSAL not in str(e):
                raise SelectOptionNotOffered(
                    f"select({element_id!r}, {value!r}) refused: {e}"
                ) from e
            raise

    def get_text(self, element_id: str, index: int = 0, *, scope: str | None = None) -> str:
        params: dict = {"id": element_id, "index": index}
        wire = self._scope_wire(scope)
        if wire:
            params["scope"] = json.dumps(wire)
        return self._get("/element/text", params)["text"]

    def is_enabled(self, element_id: str, *, scope: str | None = None) -> bool:
        """Whether element is currently enabled (clickable / not disabled).

        Per the onboarding client-target-state Done-definition (tracked
        internally), which references `test_handle_entry_outcomes.py`,
        which asserts `is_enabled` on the Continue button across the
        handle-check outcome matrix. Returns False when the element
        doesn't exist (mirrors `is_visible`).

        ⚠ **The except-False below also swallows "this bridge has no such
        endpoint"** — and until 2026-09-26 ANDROID was exactly that case
        (measured 2026-08-17: `BridgeHttpServer.kt` had no `/element/enabled`
        handler, so this returned False for EVERY element there, and an
        `assert not is_enabled(...)` passed **vacuously** while its
        `assert is_enabled(...)` twin failed). The android route exists now
        (compile-verified; a device run is still pending android's e2e
        venue), but the asymmetry is a
        property of this except-False on ANY bridge: never mark a test for an
        app on the strength of a disabled-assert alone.
        """
        params = {"id": element_id}
        scope_wire = self._scope_wire(scope)
        if scope_wire is not None:
            params["scope"] = json.dumps(scope_wire)
        try:
            return bool(self._get("/element/enabled", params=params).get("enabled", False))
        except Exception:
            return False

    def is_disabled(self, element_id: str, *, scope: str | None = None) -> bool:
        """Whether the element is present but *not* interactive — the inverse
        of `is_enabled`. The uniform read for the HTTP-bridge apps that
        implement it (web `disabled` attr, WinUI `!IsEnabled`, GTK
        `!SENSITIVE`, AppKit `!isEnabled` — all surface through the same
        `/element/enabled` contract, and android's `UiObject2.isEnabled` since
        2026-09-26 — before that the android bridge served no such endpoint
        and this read True for every element there; see the warning on
        `is_enabled`). Callers should query an element
        they know is rendered (a disabled widget is still SHOWING); a missing
        element reads as not-enabled, so `is_disabled` is True for it too.
        """
        return not self.is_enabled(element_id, scope=scope)

    def is_visible(self, element_id: str, *, scope: str | None = None) -> bool:
        params: dict = {"id": element_id}
        wire = self._scope_wire(scope)
        if wire:
            params["scope"] = json.dumps(wire)
        return self._get("/element/visible", params)["visible"]

    def press_key(self, element_id: str, key: str, *, scope: str | None = None) -> None:
        body: dict = {"id": element_id, "key": key}
        wire = self._scope_wire(scope)
        if wire:
            body["scope"] = wire
        self._post_with_scroll("/element/key", body)

    def get_attr(
        self, element_id: str, attribute: str, index: int = 0, *, scope: str | None = None
    ) -> str | None:
        params: dict = {"id": element_id, "attr": attribute, "index": index}
        wire = self._scope_wire(scope)
        if wire:
            params["scope"] = json.dumps(wire)
        try:
            resp = self._get("/element/attr", params)
        except LookupError:
            return None
        value = resp.get("value")
        return value if value is None else str(value)

    def wait_for(self, element_id: str, timeout: float = 10.0, *, scope: str | None = None) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.is_visible(element_id, scope=scope):
                return
            # Targeted scroll-into-view (supporting bridges only) — brings an
            # offscreen-but-realized element into the viewport without the
            # page-jump overshoot of blind scroll(). No-op elsewhere.
            if self._scroll_into_view(element_id, scope=scope) and self.is_visible(
                element_id, scope=scope
            ):
                return
            time.sleep(0.3)
        # Element not found within timeout — try scrolling to bring it into view.
        # GTK/SwiftUI/WinUI lazily render offscreen elements, so they may not
        # appear in the accessibility tree until scrolled into the viewport.
        for _ in range(self._MAX_SCROLL_RETRIES):
            self.scroll("down")
            if self.is_visible(element_id, scope=scope):
                return
        # e2e rule 6 — fold a diagnose() snapshot into the timeout so EVERY
        # wait_for failure classifies itself (never-rendered count=0 vs.
        # rendered-but-hidden visible=False,count>=1) without a debugger re-run.
        # Failure-path-only: a wait_for that ever sees the element returns above,
        # so a passing test never reaches this branch. diagnose() is fully guarded.
        raise TimeoutError(
            f"Element '{element_id}' not visible after {timeout}s (including scroll); "
            f"{self.diagnose(element_id, scope=scope)}"
        )

    def wait_until_enabled(
        self, element_id: str, timeout: float = 10.0, *, scope: str | None = None
    ) -> None:
        """Wait until `element_id` is visible AND enabled, then return.

        `wait_for` only proves the element RENDERED. A control that is visible
        but transiently disabled — `!vm.isBusy`, `!form.canCreate`,
        `!vm.creatingFeed` — is the normal state right after the view appears, so
        the ubiquitous `wait_for(id)` + `click(id)` pair drives a control no user
        could have clicked yet. web never had this bug: Playwright's `click()`
        auto-waits for actionability. This is that wait, made explicit for the
        bridge drivers, and it is what a real user does — see a disabled button,
        wait a moment, then click.

        Use it before clicking any control whose registration carries an
        `isEnabled:` predicate. Deliberately NOT folded into `click()` itself:
        some tests drive a *permanently* disabled control on purpose (asserting
        the refusal — `tests/test_apple_disabled_actuation.py`), and those must
        stay immediate rather than burn a full timeout budget on every call.
        """
        deadline = time.monotonic() + timeout
        self.wait_for(element_id, timeout=timeout, scope=scope)
        while time.monotonic() < deadline:
            if self.is_enabled(element_id, scope=scope):
                return
            time.sleep(0.2)
        # Convention 6 — the failure diagnoses itself. A control still disabled
        # at the deadline is a REAL finding (the app never became ready), and it
        # must not be confused with "element missing", which `wait_for` above
        # already ruled out.
        # ⚠ Diagnose WITHOUT `attrs=('enabled',)`. That reads `get_attr(id,
        # "enabled")` — the generic attribute map — which is a DIFFERENT
        # endpoint from the `/element/enabled` this wait polls, and which linux
        # and windows do not carry the key in. It printed `enabled=None`, which
        # reads as "this platform doesn't surface enabled-ness" and so invites
        # the reader to dismiss a real finding as a harness gap (measured
        # 2026-08-28 on the mail-aliases deadlock, whose
        # failure message said exactly that while linux served the endpoint
        # correctly all along). The authoritative read is `is_enabled`, so
        # report that.
        raise TimeoutError(
            f"Element '{element_id}' is visible but still DISABLED after "
            f"{timeout}s — the app never became ready to accept this action; "
            f"{self.diagnose(element_id, scope=scope)}, "
            f"is_enabled={self.is_enabled(element_id, scope=scope)!r}"
        )

    def count(self, element_id: str, *, scope: str | None = None) -> int:
        params: dict = {"id": element_id}
        wire = self._scope_wire(scope)
        if wire:
            params["scope"] = json.dumps(wire)
        return self._get("/element/count", params)["count"]

    # --- Bulk indexed reads ---

    # Bridges that serve GET /element/texts and /element/attrs — ONE find over
    # the frame, then N property reads — opt in by setting this True. Default
    # False so a bridge without the routes never pays a 404 round trip per
    # call (the _supports_scroll_into_view rule). It is a code-level
    # declaration on the driver CLASS, deliberately not an env var or a
    # runtime flag: the fast path is the only path on a bridge that has it,
    # so there is no configuration for a test to get wrong.
    _supports_bulk_reads = False

    def get_texts(self, element_id: str, *, scope: str | None = None) -> list[str]:
        """One round trip for the whole column — see
        :meth:`PlatformDriver.get_texts`."""
        payload = self._bulk_read("/element/texts", element_id, scope=scope)
        if payload is None:
            return super().get_texts(element_id, scope=scope)
        return [("" if t is None else str(t)) for t in payload.get("texts", [])]

    def get_attrs(
        self, element_id: str, attribute: str, *, scope: str | None = None
    ) -> list[str | None]:
        """One round trip for the whole column — see
        :meth:`PlatformDriver.get_attrs`."""
        payload = self._bulk_read(
            "/element/attrs", element_id, scope=scope, extra={"attr": attribute}
        )
        if payload is None:
            return super().get_attrs(element_id, attribute, scope=scope)
        return [(v if v is None else str(v)) for v in payload.get("values", [])]

    def _bulk_read(self, path: str, element_id: str, *, scope: str | None = None,
                   extra: dict | None = None) -> dict | None:
        """The shared bulk-route call. ``None`` means "fall back to the
        per-element loop" — either this driver declares no support, or the
        bridge 404'd the route.

        A 404 here is unambiguous because a bulk route answers an EMPTY LIST
        for an element that isn't on screen (0 matches is a real answer, not a
        miss), so the only thing a 404 can mean is "this bridge has no such
        route" — an app binary built before the route landed, which a partial
        roll-out makes routine. Falling back keeps that app correct at the old
        speed instead of reddening it; every other bridge error propagates,
        because a bridge that HAS the route and fails to serve it is a real
        defect and hiding it behind a slow path is how a regression survives.
        """
        if not self._supports_bulk_reads:
            return None
        params: dict = {"id": element_id, **(extra or {})}
        wire = self._scope_wire(scope)
        if wire:
            params["scope"] = json.dumps(wire)
        try:
            return self._get(path, params)
        except LookupError:
            return None

    def screenshot(self, name: str) -> Path:
        result = self._post("/screenshot", {"name": name})
        return Path(result["path"])

    def tree(self) -> str:
        """Full accessibility-tree dump from the bridge — same data Xcode's
        Accessibility Inspector exposes (where the bridge implements it;
        currently apple-bridge only). Use for debugging "ID was set in
        SwiftUI/SwiftUI-equivalent but the bridge can't find it" puzzles.

        Bridges that don't implement /tree return an empty string.
        """
        try:
            return self._get("/tree", {}).get("tree", "")
        except Exception:
            return ""

    def registry_snapshot(self) -> list[dict] | None:
        """``GET /registry`` — see :meth:`PlatformDriver.registry_snapshot`.

        A bridge without the route answers ``None`` (the base class's "no such
        surface"), never ``[]``: a 404 swallowed into an empty list would let a
        checker report a clean frame it never read. The read is deliberately
        unguarded past that — a bridge that HAS the route and fails to answer it
        is a real defect, and swallowing it would hide exactly the thing the
        route exists to make visible.
        """
        try:
            payload = self._get("/registry", {})
        except Exception:
            return None
        elements = payload.get("elements")
        return elements if isinstance(elements, list) else None

    def dismiss_system_dialogs(self) -> int:
        """Dismiss system dialogs (e.g. Windows Firewall). Returns count dismissed."""
        result = self._post("/dismiss-dialogs", {})
        return result.get("dismissed", 0)

    def set_input_files(self, element_id: str, files: str | list[str]) -> None:
        """Attach file(s) via the compose state protocol.

        Native bridges (AT-SPI, FlaUI, Apple) can't control file picker dialogs.
        Instead, set_state({"compose": {"file": path, "target": element_id}})
        tells the test agent to stage it directly against whichever real
        production call the ``element_id``'s file-chooser callback would have
        made — feed's ``compose-file`` calls ``stage_attachment`` (deferred read
        at post time); conversations' ``attachment-button`` calls
        ``add_attachment``/``add_new_thread_attachment`` (bytes read now,
        against whichever composer is active) — so ``target`` lets the agent
        disambiguate. Web driver overrides this with Playwright's real
        set_input_files(); android's overrides it to put the file ON THE
        DEVICE first (``AndroidBridgeDriver.push_input_file``), since its agent
        cannot open a pytest-host path.
        """
        path = files if isinstance(files, str) else files[0]
        self.set_state({"compose": {"file": path, "target": element_id}})

    def navigate_to(self, view: str) -> None:
        """Navigate via the state protocol — reliable on all bridge-backed clients."""
        self.set_state({"nav": {"stack": [{"view": view}]}})

    # --- Test State Protocol ---

    def set_state(self, state: dict, timeout: float | None = None,
                  *, wait_ready: bool = True) -> None:
        """Send a state patch command and wait for the app to acknowledge it.

        Waits for both command acknowledgment (last_command_id match) AND
        UI readiness (ready == true). The ready flag is set to false when
        a deferred UI action (e.g. navigation) is queued, and true after
        the UI thread completes it. Agents that don't send ready are
        treated as always ready (backward compatible).

        ``wait_ready=False`` drops the second half and returns on the ack
        alone. Exactly one caller needs it, and it is not an optimization: a
        page whose fetch the test is deliberately HOLDING pending (see
        ``helpers/rpc_hold``) never reaches ``ready``, because on some apps the
        nav edge awaits that very fetch before the page can render at all
        (tui's ``Op::FetchPrivacy``). Waiting for readiness there would block
        the test on the state it exists to observe, and it would time out
        rather than assert. The ack still proves the command was delivered, and
        the caller then waits on a real observable of its own — never a sleep.
        """
        import uuid
        if timeout is None:
            timeout = BRIDGE_ACK_BUDGET_S
        # Remember the authenticated identity so an in-process `hard_reload` can
        # replay it after a relaunch (see __init__ / InProcessAgentDriver.hard_reload).
        if isinstance(state, dict) and isinstance(state.get("session"), dict):
            self._last_session = state["session"]
            # …and hand a relaunched store back the machine's principal slot for
            # this actor before the app sees the sign-in (the principal-slot carry),
            # and — on an app whose install device secret is a row in that same
            # store — the secret beside it, for the reason that method's doc gives.
            self._restore_principal_slot(state["session"])
            self._restore_install_device_secret_row(state["session"])
        cmd_id = f"cmd_{uuid.uuid4().hex[:8]}"
        self._post("/app/commands", {
            "id": cmd_id,
            "action": "patch",
            "state": state,
        })
        # Poll until the app acknowledges AND is ready
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                resp = self._get("/app/state")
                if (resp.get("last_command_id") == cmd_id
                        and (not wait_ready or resp.get("ready", True))):
                    return
            except Exception:
                pass
            time.sleep(0.2)
        raise TimeoutError(f"App did not acknowledge command {cmd_id} within {timeout}s")

    def get_state(self, path: str | None = None, *,
                  wait_for=None, timeout: float = 5.0) -> dict | None:
        """Read the app's current state. Returns None if no state yet.

        Args:
            path: dot-separated path like "session.authenticated" to read a nested value.
            wait_for: optional predicate (callable taking state dict, returning bool).
                      If provided, polls until the predicate is satisfied or timeout.
                      Native apps have async state serialization (50-150ms stale window),
                      so polling is needed after UI actions. Web overrides this to skip polling.
            timeout: seconds to wait for the predicate (only used when wait_for is set).
        """
        if wait_for is not None:
            deadline = time.monotonic() + timeout
            result = None
            while time.monotonic() < deadline:
                result = self._get_state_raw(path)
                if result is not None and wait_for(result):
                    return result
                time.sleep(0.2)
            return result  # return last state even if predicate not met
        return self._get_state_raw(path)

    def _get_state_raw(self, path: str | None = None) -> dict | None:
        """Internal: single state read without polling."""
        try:
            resp = self._get("/app/state")
        except Exception:
            return None
        if not resp:
            return None
        state = resp.get("state", resp)
        if path:
            for key in path.split("."):
                if isinstance(state, dict):
                    state = state.get(key)
                else:
                    return None
        return state

    def call_command(self, action: str, payload: dict | None = None,
                     timeout: float | None = None) -> str | None:
        """Send an arbitrary bridge command and wait for the app to ack it.

        Used by domain action layers (e.g. conversations) that route directly
        to a domain-owned singleton (`ConversationsManagerHost.Instance`)
        instead of going through the OnboardingMachine. The TestAgent's
        `ProcessCommand` switch dispatches `action` to the matching handler.
        Mirrors `call_machine_method` but generic over the action name.

        Returns the command's `state.machine_method_result`, exactly as
        `call_machine_method` does — a command that produces a value (e.g.
        `custodian_pull_run_now`) is read from the return, and the overwhelming
        majority that produce none read ``None``. The value is this command's
        and never a stale neighbour's: the agent stashes the result immediately
        before setting `last_command_id` (fauna-tui `main.rs`), and a command
        that returns nothing stashes ``None``, so the slot is rewritten on every
        command rather than accumulating.
        """
        import uuid
        if timeout is None:
            timeout = BRIDGE_ACK_BUDGET_S
        cmd_id = f"cmd_{uuid.uuid4().hex[:8]}"
        body = {"id": cmd_id, "action": action}
        if payload:
            body.update(payload)
        self._post("/app/commands", body)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                resp = self._get("/app/state")
                if (resp.get("last_command_id") == cmd_id
                        and resp.get("ready", True)):
                    return resp.get("state", {}).get("machine_method_result")
            except Exception:
                pass
            time.sleep(0.2)
        raise TimeoutError(
            f"App did not acknowledge command {action} within {timeout}s"
            f"{self.ack_timeout_diagnostics()}"
        )

    def barrier(self, timeout: float | None = None) -> None:
        """Convention 14's causal anchor, over the shared `/app/commands` rails.

        The ack protocol already carries the ordering: the agent sets
        `last_command_id` only after applying the command on its UI thread, so
        polling for this command's id *is* the barrier — provided the app's
        `barrier` arm does its own per-platform ordering work first (tui drains
        its `UiMessage` channel; linux defers this very ack into a glib idle).
        Both are pinned by `test_agent_barrier.py`.
        """
        self.call_command("barrier", timeout=timeout)

    def call_machine_method(self, name: str, json_arg: str = "",
                            timeout: float | None = None) -> object:
        """Invoke a named OnboardingMachine method through the app's bridge.

        Used by `tests/e2e-unified/drivers/machine_test_setter.py` to fixture
        wizard snapshots via `set_handle_check_snapshot_for_test` /
        `set_invite_request_snapshot_for_test`. The bridge surface is the
        E2E bridge contract (tracked internally); the app must be linked with
        the `test-helpers` feature for the named methods to exist.

        Returns the method's result: **reader** names (`provisioning_snapshot`,
        `provider_base_url`) hand back the JSON their agent stashed under
        `state.machine_method_result`; **setter** names return ``None``. That
        mirrors web's value-returning `__fauna_callMachineMethod` and the
        shared `OnboardingMachine::call_machine_method_with_result` dispatcher
        every native agent delegates to. A client whose agent doesn't stash a
        result simply reads ``None`` — the setter path, which is all most tests
        use.
        """
        import uuid
        if timeout is None:
            timeout = BRIDGE_ACK_BUDGET_S
        cmd_id = f"cmd_{uuid.uuid4().hex[:8]}"
        self._post("/app/commands", {
            "id": cmd_id,
            "action": "call_machine_method",
            "method": name,
            "json_arg": json_arg,
        })
        # The ack barrier guarantees the published state reflects this command's
        # effects (the agent rebuilds its state object before setting
        # last_command_id), so the result we read is this command's.
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                resp = self._get("/app/state")
                if (resp.get("last_command_id") == cmd_id
                        and resp.get("ready", True)):
                    return resp.get("state", {}).get("machine_method_result")
            except Exception:
                pass
            time.sleep(0.2)
        raise TimeoutError(
            f"App did not acknowledge call_machine_method {name} within {timeout}s"
            f"{self.ack_timeout_diagnostics()}"
        )

    def set_provider_base_urls(self, urls: dict[str, str]) -> None:
        """Redirect the OnboardingMachine's provider HTTP base URLs (vps/dns/
        nest) at a local fake / test nest, for every bridge-backed native app.

        Native twin of web's query-param+reload path: routes through the shared
        `call_machine_method` bridge to the machine's runtime
        `set_provider_base_urls` setter, which overrides the in-place machine (no
        reconstruction — the override is read dynamically at probe time). Lifted
        here so linux / windows / android / macOS / iOS (all `HttpBridgeDriver`
        subclasses, in-process or not) share one implementation; web overrides it
        with its reload-based path. Track 2 of that effort.
        """
        import json
        self.call_machine_method("set_provider_base_urls", json.dumps(urls))

    def reset(self, timeout: float = 10.0) -> None:
        """Reset the app to factory state (clear stores, return to onboarding).

        Waits for the app to both acknowledge the command AND settle into
        unauthenticated state.  On iOS the SwiftUI .task modifiers re-fire
        after resetToFactory(), so the command ack arrives before the UI is
        truly ready.  Waiting for session.authenticated==false avoids a race
        where a subsequent set_state() collides with the re-initialization.
        """
        # Per-test boundary: drop any preserve_state_across_relaunch() pin so this
        # session-scoped driver's next relaunch starts from a fresh store unless the
        # incoming test re-pins (see _clear_relaunch_pin).
        self._clear_relaunch_pin()
        # …and forget the session an earlier test injected. A reset returns the app
        # to factory state, so that session no longer exists in it; left cached, a
        # later `hard_reload()` in a test that never signed in replays another
        # test's login over its own store. A test that signs in after this
        # re-records its session through `set_state`.
        self._last_session = None
        import uuid
        cmd_id = f"cmd_{uuid.uuid4().hex[:8]}"
        self._post("/app/commands", {"id": cmd_id, "action": "reset"})
        deadline = time.monotonic() + timeout
        acked = False
        while time.monotonic() < deadline:
            try:
                resp = self._get("/app/state")
                if resp.get("last_command_id") == cmd_id:
                    acked = True
                if acked and resp.get("ready", True):
                    state = resp.get("state", resp)
                    session = state.get("session", {}) if isinstance(state, dict) else {}
                    if not session.get("authenticated", False):
                        return
            except Exception:
                pass
            time.sleep(0.2)
        if not acked:
            raise TimeoutError(f"App did not acknowledge reset within {timeout}s")
        raise TimeoutError(f"App acknowledged reset but session still authenticated after {timeout}s")

    def logout(self, timeout: float = 10.0) -> None:
        """Clear session but keep data, return to onboarding."""
        import uuid
        cmd_id = f"cmd_{uuid.uuid4().hex[:8]}"
        self._post("/app/commands", {"id": cmd_id, "action": "logout"})
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                resp = self._get("/app/state")
                if resp.get("last_command_id") == cmd_id and resp.get("ready", True):
                    return
            except Exception:
                pass
            time.sleep(0.2)
        raise TimeoutError(f"App did not acknowledge logout within {timeout}s")

    def wait_for_state(self, predicate, timeout: float = 10.0) -> dict:
        """Poll until the app state satisfies predicate. Returns the matching state."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            state = self.get_state()
            if state is not None and predicate(state):
                return state
            time.sleep(0.2)
        raise TimeoutError(f"State predicate not satisfied within {timeout}s")


class _ElementStub:
    """Minimal stub for code that calls find_element directly."""

    def __init__(self, driver: HttpBridgeDriver, element_id: str, index: int, scope: str | None = None):
        self._driver = driver
        self._id = element_id
        self._index = index
        self._scope = scope

    def click(self):
        self._driver.click(self._id, self._index, scope=self._scope)

    @property
    def text(self):
        return self._driver.get_text(self._id, self._index, scope=self._scope)

    def is_displayed(self):
        return self._driver.is_visible(self._id, scope=self._scope)
