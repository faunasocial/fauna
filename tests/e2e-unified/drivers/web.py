"""Web E2E driver backed by the Playwright automation bridge.

Spawns the web bridge process and communicates via the standard
bridge HTTP API. Web-specific operations (set_input_files) are
exposed through extra bridge endpoints.

The state protocol (set_state, get_state, reset, logout) is handled
via the JS test agent injected at session start.
"""

from __future__ import annotations

import base64
import json as json_mod
import os
import subprocess
import sys
import time
from pathlib import Path
from urllib.parse import urlparse

from .http_bridge import HttpBridgeDriver
from .port_util import (drain_pipes, popen_group_kwargs, reap_descendants_of,
                        terminate_tree, track_process, untrack_process)

_BRIDGE_SCRIPT = Path(__file__).parent.parent / "web-bridge" / "server.py"
_AGENT_JS = Path(__file__).parent.parent / "web-bridge" / "agent.js"

# Ceiling for any call that can absorb a FULL SPA boot. Two shapes hit it:
# the calls that *initiate* one — `POST /session` (launches Chromium + first
# navigate), `_fresh_boot`'s `location.reload()` / `POST /page/relaunch` — and,
# just as heavy, the FIRST page-bound call issued *after* a navigation
# (`_ensure_agent`'s typeof probe): a JS evaluate cannot run until the browser
# main thread frees, and under machine load the wasm re-parse + re-execute pegs
# it for the whole boot, so that "trivial" evaluate inherits the entire boot
# cost — and stalls the single-threaded bridge thread with it. 400s: observed
# boots under this box's 20+-session load exceeded 210s (2026-08-02 — a 120s
# RPC ceiling plus a 90s health poll both expired mid-boot), and convention 14
# sizes ceilings far above the worst non-pathological delay — a healthy box
# returns in a few seconds and never pays this. A timeout is no longer misread
# as a dead bridge anywhere — the base `_post`/`_get` classify a socket timeout
# via a /health deadline poll, and only a refused/reset connection concludes
# death (http_bridge._timeout_verdict; the false-"[BRIDGE DEAD ... (setup)]"
# family this ceiling used to be the only defense against).
HEAVY_BOOT_TIMEOUT_S = 400.0

# Mirrors `fauna_e2e_agent::FOCUS_MOVE_MAX_TIMES` — the shared upper bound on
# both `focus_move`'s `times` and the number of real key presses `switch_pane`
# will throw at the real door before concluding the target region is
# unreachable (the `barrier-e2e.ts` idiom: re-derive the payload/loop rulings
# on the Python side rather than importing the Rust crate, and say so).
_FOCUS_MOVE_MAX_TIMES = 256

# The browser is Playwright's bundled Chromium (resolved bridge-side via
# `drivers/browser.py`) — no machine-wide singleton, so concurrent web drivers
# (sibling sessions, tier_4 smokes, even a second driver in this process) need
# no serialization. The snap-Chromium flock that used to live here
# (`drivers/chromium_lock.py`) was removed 2026-07-14 with the singleton it
# guarded; see testing.md § Cross-app e2e conventions, point 9.


class WebBridgeDriver(HttpBridgeDriver):
    """Web E2E driver backed by Playwright automation bridge.

    Launches a Python HTTP bridge process that runs Playwright internally,
    then communicates via the same JSON API as the FlaUI bridge.
    """

    # web's leaf is the browser language's region subtag.
    REGION_SOURCE_KEY = "source_browser_locale"

    #: The escrow-trust seed's name — the native environment variable, and on
    #: web the `localStorage` key (`fauna_client_core::nest_trust::
    #: E2E_TRUST_NEST_IDENTITY`).
    _TRUST_SEED_KEY = "FAUNA_E2E_TRUST_NEST_IDENTITY"

    def declare_region_for_relaunch(self, region: str, registry_hex: str) -> bool:
        """Web's twin of the native environment seam: a page reads no
        environment, so the two keys go into the origin's ``localStorage``,
        which ``WasmRegionPlane`` reads at open under ``test-helpers``
        (``libs/fauna-wasm/src/region.rs::e2e_overrides``) and which survives
        the reload ``recover()`` performs."""
        registry_key, declared_key = self._REGION_ENV_KEYS
        self._execute_js(
            "(() => {"
            f"  localStorage.setItem({json_mod.dumps(registry_key)}, {json_mod.dumps(registry_hex)});"
            f"  localStorage.setItem({json_mod.dumps(declared_key)}, {json_mod.dumps(region)});"
            "  return true;"
            "})()"
        )
        return True

    def clear_region_declaration(self) -> None:
        """Undo :meth:`declare_region_for_relaunch` for the next relaunch."""
        if self._app_killed:
            return
        keys = json_mod.dumps(list(self._REGION_ENV_KEYS))
        self._execute_js(f"(() => {{ for (const k of {keys}) localStorage.removeItem(k); return true; }})()")

    def log_scope_across_relaunch(self) -> str:
        """``"evicting"`` — the console ring holds 500 entries and drops the oldest, so absence is never evidence - see `console_log`'s own eviction warning.

        See the base declaration for what each answer means and why it is
        declared rather than inferred; pinned per driver by
        `tests/test_module_relaunch.py`.
        """
        return "evicting"

    def __init__(self):
        super().__init__()
        self._bridge_proc: subprocess.Popen | None = None
        self._agent_code: str | None = None
        # Multi-page support: every request addresses one bridge page (page id
        # "1" = the page this driver launched). A twin driver (`open_twin_page`)
        # shares the bridge + Chromium but drives its own page in an ISOLATED
        # BrowserContext — the cheap second web *device* (one browser, two
        # contexts). A second full driver is also safe (bundled Chromium has no
        # machine-wide singleton); the twin is preferred, not load-bearing.
        self._page_id: str = "1"
        self._spa_url: str | None = None
        self._twin_of: "WebBridgeDriver | None" = None
        # True between an unclean kill (page closed, context+localStorage kept) and
        # the relaunch that reopens the page — the crash-recovery journeys' contract
        # (see supports_unclean_kill / kill_uncleanly / recover below).
        self._app_killed: bool = False

    def is_web(self) -> bool:
        return True

    def is_nav_tab_revealed(self, element_id: str, *, scope: str | None = None) -> bool:
        """Web's gated nav tabs (``admin-tab`` / ``family-tab`` in
        ``routes/+layout.svelte``) are Svelte ``{#if}`` blocks: ABSENT from the
        DOM until their gate fires, PRESENT once revealed — the same
        Collapsed-until-revealed shape ``WindowsDriver`` already overrides this
        for, and for the identical reason: the base ``is_visible_scrolled``
        calls ``scroll_to`` first, which on web is a Playwright
        ``scroll_into_view_if_needed`` that waits on the locator to *appear* —
        for a genuinely absent element that wait runs the full bridge timeout
        and 500s (`test_admin_tab_hidden_for_non_admin[web]`), it does not
        return False quickly. Tree-membership is the honest, hang-free signal
        here, exactly as ``WindowsDriver.is_nav_tab_revealed`` documents."""
        return self.count(element_id, scope=scope) >= 1

    # The web bridge serves POST /element/scroll-into-view (Playwright
    # scroll_into_view_if_needed), so `scroll_to` works on web too.
    _supports_scroll_into_view = True

    # --- Page routing: every bridge call names this driver's page ---

    def _post(self, path: str, data: dict | None = None,
              timeout: float | None = None) -> dict:
        # timeout=None → the base resolves the shared generous default at call
        # time; a literal default here would silently shadow it.
        body = dict(data or {})
        body.setdefault("page", self._page_id)
        return super()._post(path, body, timeout=timeout)

    def _get(self, path: str, params: dict | None = None) -> dict:
        merged = dict(params or {})
        merged.setdefault("page", self._page_id)
        return super()._get(path, merged)

    def open_twin_page(self) -> "WebBridgeDriver":
        """Open a SECOND page in its own isolated BrowserContext (fresh
        localStorage/IndexedDB) on this driver's bridge and return a full
        driver bound to it — the web analogue of launching a second native
        app with a fresh data dir (``alice_second_device``). The twin's
        ``teardown()`` closes only its page; the bridge, Chromium, and the
        machine-wide chromium flock stay owned by this (primary) driver."""
        resp = self._post("/page", {"url": self._spa_url or ""})
        twin = WebBridgeDriver()
        twin._url = self._url
        twin._page_id = str(resp["page_id"])
        twin._agent_code = self._agent_code
        twin._spa_url = self._spa_url
        twin._twin_of = self
        twin._inject_agent()
        return twin

    def open_same_context_tab(self) -> "WebBridgeDriver":
        """Open a second page in THIS driver's own BrowserContext and return a
        full driver bound to it — a second **tab**, not a second device.

        The difference from ``open_twin_page`` is the whole point. A twin gets a
        fresh context (its own ``localStorage``/``IndexedDB``), which is the web
        analogue of a second machine; this shares one origin store, exactly as
        two real tabs of one browser profile do. Web's concurrent-instances leg
        (``account-scoping.md`` § Concurrent instances → *Web*) is defined
        against precisely that sharing — the per-tab account pin exists because
        the identity slots underneath are common to every tab — so a twin cannot
        witness it: with nothing shared there is nothing for a pin to isolate,
        and the test would pass against no mechanism at all.

        Teardown closes only this tab's page; the shared context (and therefore
        the other tab's store) stays owned by the driver that created it.
        """
        resp = self._post("/page", {"url": self._spa_url or "", "share_context": self._page_id})
        tab = WebBridgeDriver()
        tab._url = self._url
        tab._page_id = str(resp["page_id"])
        tab._agent_code = self._agent_code
        tab._spa_url = self._spa_url
        tab._twin_of = self
        tab._inject_agent()
        return tab

    def launch(self, config: dict) -> None:
        url = config["url"]
        self._spa_url = url

        # Cache agent JS for re-injection
        if _AGENT_JS.exists():
            self._agent_code = _AGENT_JS.read_text()

        # Start the bridge using the same Python that's running the tests,
        # so it inherits the venv with playwright installed. Group-leader +
        # die-with-parent spawn: the bridge (and thus its playwright node +
        # Chromium subtree) is reapable as one unit and dies with this run
        # even on `kill -9` of pytest (port_util.popen_group_kwargs).
        python = sys.executable
        self._bridge_proc = subprocess.Popen(
            [python, str(_BRIDGE_SCRIPT)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            **popen_group_kwargs(),
        )
        # Windows half of the same guarantee (no process groups there, so
        # `popen_group_kwargs()` is `{}`): bind the bridge AND every descendant
        # it later spawns to a kill-on-close job. The grandchildren are the
        # whole point here — the playwright node and its Chromium subtree are
        # exactly the "grandchild outlives the run" case `terminate_tree`
        # cannot reach (testing.md § conventions, point 9). No-op off Windows.
        reap_descendants_of(self._bridge_proc.pid)
        track_process(self._bridge_proc)

        # Read the port from stdout
        port = None
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if self._bridge_proc.poll() is not None:
                stderr = self._bridge_proc.stderr.read()
                raise RuntimeError(f"Web bridge exited early. stderr: {stderr[:500]}")
            line = self._bridge_proc.stdout.readline()
            if line.startswith("BRIDGE_PORT="):
                port = int(line.strip().split("=")[1])
                break
        if port is None:
            terminate_tree(self._bridge_proc)
            stderr = self._bridge_proc.stderr.read()
            raise RuntimeError(f"Web bridge didn't report port. stderr: {stderr[:500]}")

        # Drain for the rest of the run — the handshake above stops reading at
        # "BRIDGE_PORT=", and an unread PIPE blocks the writer once its ~64 KB
        # buffer fills (port_util.drain_pipes has the full mechanism + the
        # seven-session windows case study).
        self._bridge_log = drain_pipes(self._bridge_proc)

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

        # Create session and navigate to the URL (heavyweight: launches Chromium
        # + loads the SPA — see HEAVY_BOOT_TIMEOUT_S for why it gets a generous
        # ceiling instead of the default 30 s that false-"BRIDGE DEAD"s under load).
        session_body = {"url": url}
        # Optional BCP-47 locale (`config["locale"]`) — the browser-context
        # twin of the native drivers' `config["environment"]` locale
        # override: web's own locale week-start
        # probe reads `navigator.language`, which only a context-level
        # locale can set.
        if config.get("locale"):
            session_body["locale"] = config["locale"]
        self._post("/session", session_body, timeout=HEAVY_BOOT_TIMEOUT_S)

        # Inject test agent
        self._inject_agent()

        # The escrow-trust seed (`_apply_r14_trust_env`), web's twin of the
        # native launch environment: a page reads no environment, so the seed
        # goes into the origin's `localStorage` under the variable's own name,
        # where the SPA's account runtime reads it under `test-helpers`
        # (`libs/fauna-wasm/src/account_runtime.rs::trusted_escrow_holders`).
        # Written before any sign-in, so the runtime the sign-in starts
        # already trusts this nest's escrow receipts; it survives reloads.
        seed = (config.get("environment") or {}).get(self._TRUST_SEED_KEY)
        if seed:
            self._execute_js(
                "(() => {"
                f"  localStorage.setItem({json_mod.dumps(self._TRUST_SEED_KEY)}, {json_mod.dumps(seed)});"
                "  return true;"
                "})()"
            )

    def teardown(self) -> None:
        # A twin owns only its page: close it and leave the bridge, Chromium,
        # and the chromium flock to the primary driver it was opened from.
        if self._twin_of is not None:
            try:
                self._delete(f"/page?page={self._page_id}")
            except Exception:
                pass
            self._twin_of = None
            return
        try:
            self._delete("/session")
        except Exception:
            pass
        if self._bridge_proc:
            # Group-wide TERM→KILL: takes the playwright node + Chromium
            # subtree down with the bridge, not just the bridge python.
            terminate_tree(self._bridge_proc)
            untrack_process(self._bridge_proc)
            self._bridge_proc = None

    # --- Agent injection ---

    def _inject_agent(self) -> None:
        """Inject the test agent JS into the current page. Boot-class ceiling:
        this often runs right after a navigation, against a main thread the
        SPA boot may still be pegging (see HEAVY_BOOT_TIMEOUT_S)."""
        if self._agent_code:
            self._execute_js(self._agent_code, timeout=HEAVY_BOOT_TIMEOUT_S)

    def _ensure_agent(self) -> None:
        """Re-inject agent if lost, and wait for SPA hydration (stores available).

        This is the designated post-boot barrier: after any navigation it is
        the first page-bound call, so its evaluate is the one that waits out
        the whole SPA boot on a loaded box — it gets the boot-class ceiling,
        never the shared RPC default (see HEAVY_BOOT_TIMEOUT_S)."""
        has_agent = self._execute_js(
            "typeof window.__faunaTestAgent !== 'undefined'",
            timeout=HEAVY_BOOT_TIMEOUT_S,
        )
        if not has_agent:
            self._inject_agent()
        # Wait for Svelte stores to be available (SPA hydration complete)
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            ready = self._execute_js(
                "typeof window.__faunaTestAgent !== 'undefined' "
                "&& typeof window.__fauna_stores !== 'undefined'",
                timeout=HEAVY_BOOT_TIMEOUT_S,
            )
            if ready:
                return
            time.sleep(0.3)
        # Stores may never appear if SPA build doesn't include the export —
        # proceed anyway; agent will fall back to raw localStorage.

    # --- Internal JS execution (not public API) ---

    def _execute_js(self, script: str, arg=None, timeout: float | None = None):
        """Execute JavaScript in the browser context (internal use only)."""
        return self._post("/execute", {"script": script, "arg": arg}, timeout=timeout).get("result")

    # --- Web-specific operations ---

    def clear_and_type(self, element_id: str, text: str, *, scope: str | None = None) -> None:
        """Clear and fill — uses the /element/fill endpoint for atomic operation."""
        body: dict = {"id": element_id, "text": text}
        wire = self._scope_wire(scope)
        if wire:
            body["scope"] = wire
        self._post("/element/fill", body)

    def get_clipboard_text(self) -> str | None:
        """The browser clipboard's text, read through the page's own async
        clipboard API by the bridge's GET /clipboard/text (which grants the
        `clipboard-read` permission and focuses the page first). ``None`` when
        nothing has been copied — the windows driver's contract."""
        return self._get("/clipboard/text").get("text")

    def set_input_files(self, element_id: str, files: str | list[str]) -> None:
        """Set file(s) on an input[type=file] element."""
        if isinstance(files, str):
            files = [files]
        self._post("/element/set_input_files", {"id": element_id, "files": files})

    def download_via_click(self, element_id: str, index: int = 0, *, scope: str | None = None) -> bytes:
        """Click `element_id` and return the bytes of the browser download it
        triggers (e.g. `snapshot-file-download-button`, backups.md § Single-file
        byte download). Web-only — native apps have no browser-download
        concept; a per-app save-as flow would need its own capture path.
        Playwright's `expect_download` must wrap the click itself server-side
        (`/element/download`), so this can't be composed from plain `click()`."""
        body: dict = {"id": element_id, "index": index}
        wire = self._scope_wire(scope)
        if wire:
            body["scope"] = wire
        result = self._post("/element/download", body)
        return base64.b64decode(result["data_b64"])

    def download_dir(self) -> str | None:
        """Where the browser downloads this page started end up, saved under
        the names the page suggested.

        The web twin of the native drivers' `FAUNA_E2E_DOWNLOAD_DIR`: a browser
        has no downloads directory the page can name, so the bridge records
        every download the page starts and this call saves what has arrived so
        far (`POST /downloads/collect`). **Each call collects** — a test that
        waits for a file to appear asks again on every poll, where a native
        test may read the directory it was handed once. Unlike
        `download_via_click` it wraps no click, so it also observes a download
        that starts long after its gesture, and one that never starts."""
        return self._post("/downloads/collect").get("dir")

    # --- Test utilities: arbitrary JS eval + page reload ---
    #
    # These are web-only escape hatches for tests that need to read
    # localStorage directly (force-quit/relaunch persistence regressions)
    # or simulate a hard reload.

    def eval_js(self, script: str) -> object:
        """Evaluate a JavaScript expression in the page and return the result.

        Wrap multi-statement scripts in `(() => { ... return X; })()`.
        Web-only — other apps don't expose a JS context.
        """
        return self._execute_js(script)

    def console_log(self) -> list[str]:
        """The bridge-captured browser console + `pageerror` ring for this
        driver's page (server.py `_attach_console_capture`). Survives an
        unclean kill/relaunch (bridge markers separate the epochs), so a
        post-crash boot that dies in an unhandled promise rejection — invisible
        to every DOM/store probe — still leaves its evidence here. Web-only.

        The ring is BOUNDED (500 lines), so a journey that boots the SPA more
        than once can evict the earlier boot's lines entirely. When that has
        happened the list opens with a warning line saying how many were lost —
        absence of a line is then not evidence the line was never logged. The
        warning is deliberately part of the returned list rather than a separate
        accessor: every existing reader quotes this list into a failure message,
        and the one that draws a conclusion from an absence is exactly the one
        that must see it.
        """
        body = self._get("/page/console")
        lines = list(body.get("lines") or [])
        dropped = int(body.get("dropped") or 0)
        if dropped:
            lines.insert(0, (
                f"<⚠ {dropped} earlier console line(s) EVICTED from the "
                f"500-entry ring — this log starts mid-stream, so a line's "
                f"absence here is NOT evidence it was never logged>"
            ))
        return lines

    def app_stderr_text(self) -> str:
        """The app's own log as one string — the accessor every native driver
        answers (``linux.py``, ``tui.py``, ``macos.py``, ``windows.py``), so a
        cross-app failure diagnostic reads it without branching on the driver
        (``helpers/room_seats.py::seat_log`` and the room journeys' own
        ``_diag``). A browser page has no stderr; its log is the console ring
        :meth:`console_log` returns, eviction warning included."""
        return "\n".join(self.console_log())

    def preserve_state_across_relaunch(self) -> bool:
        """Always true on web, with nothing to do: a reload — or an unclean kill
        that keeps the BrowserContext — keeps the same Chromium profile and origin,
        so `localStorage` (where the SPA's long-term store lives) survives by
        construction — unlike the native drivers, which hand the relaunched process
        a fresh data dir unless asked not to."""
        return True

    # NOTE: no `relaunch_preserves_injected_identity()` override — web INHERITS the
    # base True, and the override that used to sit here (2026-08-05) was built on a
    # premise that was simply false. It claimed `logged_in_app`'s `set_state`
    # injection "lands in the SPA's in-memory store, never in localStorage", so a
    # reload booted signed out. The agent has written the session straight to
    # localStorage since 2026-04-03 (`web-bridge/agent.js` applyPatch's session
    # branch: `fauna_secret` / `fauna_node_url` / `fauna_handle` /
    # `fauna_registered`), and a reload re-hydrates from exactly those keys —
    # measured directly (localStorage intact, `actor_id`/`handle`/`domain` back in
    # the state, `feed-tab` visible again), and load-bearing for
    # `test_mail_sent_copy_restart_web.py`, whose whole proof is an identical
    # injected login surviving `hard_reload()`.
    #
    # What actually failed was the "am I in?" SIGNAL, not the identity:
    # `reached_authenticated_app` waited on `feed-tab`, which the Settings/admin
    # shells swap out in place (`ui/settings.md` § Navigation model), and the
    # failing test relaunches from Settings → Privacy. Fixed there, in
    # `tests/common/launch_harness.py`, for all apps at once.

    # --- Crash-recovery primitives (nest/common.md § Client-state recoverability) ---
    #
    # The web "app" is the SPA running in a Playwright page; its durable store is
    # `localStorage`, held by the page's BrowserContext (one context per page —
    # server.py `_handle_session_create`), NOT by the page. So the native
    # SIGKILL-the-process / keep-the-data-dir shape maps to: CLOSE the page (abrupt,
    # no unload flush) but KEEP the context, then reopen a page in that same context.

    def supports_unclean_kill(self) -> bool:
        """True: the bridge can close this driver's page while preserving its
        BrowserContext's localStorage (`/page/kill`), so a relaunch reads back the
        genuine durable store — not a fresh one that would make CR assertions
        vacuous (common.md § Client-state recoverability)."""
        return True

    def kill_uncleanly(self) -> None:
        """Close the SPA's page but keep its BrowserContext (localStorage survives)
        — the web crash (PlatformDriver.kill_uncleanly). `page.close()` runs no
        beforeunload/unload handler, and the SPA persists to localStorage
        synchronously (no unload backup — `wasm.ts`), so the durable state at the
        kill instant is exactly what survives. After this the app is GONE; drive no
        element until a relaunch (`hard_reload()` / `recover()`)."""
        self._post("/page/kill", {})
        self._app_killed = True

    def _fresh_boot(self) -> None:
        """Boot a fresh SPA instance over the (preserved) localStorage — the web
        relaunch. If the app was unclean-killed, reopen the page in its kept
        context; otherwise reload the live page. Then re-inject the agent and wait
        for the SPA to rehydrate."""
        if self._app_killed:
            self._post("/page/relaunch", {"url": self._spa_url or ""}, timeout=HEAVY_BOOT_TIMEOUT_S)
            self._app_killed = False
        else:
            self._execute_js("location.reload()", timeout=HEAVY_BOOT_TIMEOUT_S)
        time.sleep(0.8)
        self._inject_agent()
        self._ensure_agent()

    def hard_reload(self) -> None:
        """Force-quit + relaunch the web app: a fresh SPA boot over the same
        origin's localStorage. Reopens the page if a prior `kill_uncleanly()` closed
        it, else a full page reload. After it, the agent is re-injected and the SPA
        has rehydrated."""
        self._fresh_boot()

    def recover(self) -> bool:
        """Relaunch the app as a fresh SPA boot (the crash-recovery journeys call
        this directly as "reopen the app"; native drivers teardown+launch a fresh
        process — web reopens the page / reloads over the preserved localStorage).

        The one caller that must NOT trigger a reload is the internal bridge-death
        recovery (`http_bridge._check_bridge`, fired only when the bridge socket
        itself errored): there the page is fine and a reload would nuke a live
        test's state — so when we arrive via a bridge-dead flag with the app not
        killed, just clear the flag once the bridge answers."""
        from_bridge_death = self._bridge_dead
        # Patient, boot-class probe: the bridge thread may be pegged inside a
        # page-bound call waiting out an SPA boot — refuse only on a refused
        # connect (process gone), not on silence (see _await_health).
        if not self._await_health(HEAVY_BOOT_TIMEOUT_S):
            return False
        self._bridge_dead = False
        if from_bridge_death and not self._app_killed:
            return True
        self._fresh_boot()
        return True

    def seed_pending_factory_reset(
        self, nest_url: str, handle: str, claim_code: str
    ) -> None:
        """Web impl of the CR-2 seam (drivers/base.py): arrange a stale
        pending-factory-reset slot in the killed page's preserved-context
        localStorage, written in the REAL store format (the bridge writes the
        per-actor registry slot; the wasm `LocalStorageSecretStore` maps no
        global keys), so the launch boot-reconcile reads back a genuine slot and
        drives the identical shared-Rust `RegistryLaunchPersistence` path
        macOS/linux prove. Idempotent (leaves a client-persisted slot verbatim) — the bridge
        does the same-origin write on a throwaway page; page "1" stays dead so
        `recover()` performs the real boot-reconcile pass."""
        self._post("/page/seed-factory-reset", {
            "url": self._spa_url or "",
            "nest_url": nest_url,
            "handle": handle,
            "code": claim_code,
        })

    def set_provider_base_urls(self, urls: dict[str, str]) -> None:
        """Redirect OnboardingMachine provider HTTP calls (vps/dns/nest) at
        a local fake.

        Stashes the base-URL map in the page's `?fauna_e2e_provider_base_urls`
        query param and reloads, so the onboarding page re-mounts and reads
        the param at module load — constructing the wizard machine WITH the
        override from the start (see machine.svelte.ts). After this call the
        page's machine ref and the e2e bridge (`call_machine_method`) share
        one override instance, so seeded state and `start_provisioning` all
        hit the fake. The param survives any later `hard_reload`
        (`location.reload()` preserves the query string).
        """
        self._ensure_agent()
        # Double-encode: inner dumps → JSON blob the page JSON.parses; outer
        # dumps → a JS string literal we can splice into the snippet safely.
        blob_literal = json_mod.dumps(json_mod.dumps(urls))
        self._execute_js(
            "(() => {"
            "  const u = new URL(location.href);"
            f"  u.searchParams.set('fauna_e2e_provider_base_urls', {blob_literal});"
            "  history.replaceState(null, '', u.toString());"
            "})()"
        )
        # Re-mount so onMount's first initOnboardingMachine() picks up the
        # override (hard_reload re-injects the agent + waits for rehydrate).
        self.hard_reload()

    def enable_dns_fake_provider(self) -> None:
        """Enable the wasm fake DNS provider (the wasm twin of native's
        ``FAUNA_DNS_PROVIDER_FAKE``) so a credentialed ``DnsManagementMachine``
        verifies/publishes a ``fake-dns-ok:<zone>`` sentinel credential offline.
        Reaches ``window.__fauna_enableDnsFakeProviderForTest`` (set up in
        ``apps/fauna-web/src/routes/+layout.svelte``). Web-only; native sets the
        env var in its launch config instead.
        """
        self._ensure_agent()
        # Wrap in an async IIFE — `_execute_js` evaluates the expression at the
        # page-evaluate top level, where a bare `await` is a SyntaxError. The hook
        # ensures wasm is loaded then flips the flag; return null so the bridge has
        # a JSON-serializable result.
        self._execute_js(
            "(async () => { await window.__fauna_enableDnsFakeProviderForTest(); "
            "return null; })()"
        )

    def set_conv_poll_secs(self, secs: float | None) -> None:
        """Override the conversations receive rail's **backstop ticker** cadence —
        ``None`` restores the default. Reaches
        ``window.__fauna_setConvPollSecs`` (set up in
        ``apps/fauna-web/src/lib/e2e-automation.ts``); the web twin of native's
        ``FAUNA_CONV_POLL_SECS`` launch env var, which the SPA cannot take.

        **Why it exists.** Under the e2e agent this rail polls every 2 s, so a
        test asserting the *push* arm delivers passes whether or not the push arm
        is alive — the "unable to fail" shape (``testing.md`` § conventions,
        point 14; ``transport.md`` § Push events names this rail as the one the
        push probe deliberately avoids for exactly this reason). Muting the
        ticker leaves the push arm as the only trigger.

        **Effective immediately, including on the sleep already in flight** — the
        SPA re-arms the pending wake at the new interval, so once this returns the
        next ticker sweep is a full new interval away. A caller may therefore mute
        and *then* trigger, with no wall-clock assumption about the old cadence.

        Web-only, and deliberately so: the natives read the env var at launch, so
        their tests spell the cadence in the launch config instead.
        """
        self._ensure_agent()
        arg = "null" if secs is None else json_mod.dumps(float(secs))
        self._execute_js(f"(window.__fauna_setConvPollSecs({arg}), null)")

    def set_conv_push_suppressed(self, on: bool) -> None:
        """Switch the conversations rail's **push + reconnect arms** off (or back
        on). Reaches ``window.__fauna_setConvPushSuppressed`` (set up in
        ``apps/fauna-web/src/lib/e2e-automation.ts``); the web twin of native's
        ``FAUNA_E2E_SUPPRESS_CONV_PUSH``, whose ``conv_push_source`` is
        ``cfg(not(wasm32))`` and therefore unreachable from the SPA.

        Both arms, matching native exactly: that env var makes ``conv_push_source``
        return ``None``, and ``subscribe_reconnects()`` lives inside the same push
        source — so a suppressed native session has no reconnect sweep either.

        The mirror of :meth:`set_conv_poll_secs`. With the push arm inert the
        durable inbox-apply drain is the rail's only path, which is what makes the
        layer-5 web receive proof able to fail (``api-layers.md`` § Inbox &
        Messaging). Web-only; the natives set the env var at launch instead.
        """
        arg = "true" if on else "false"
        self._ensure_agent()
        self._execute_js(f"(window.__fauna_setConvPushSuppressed({arg}), null)")

    def enable_fake_plc_directory(self, url: str) -> None:
        """Point the wasm ATProto custody check (``critical-alerts.md`` feeder
        #1) at a fake PLC directory ``url`` — the wasm twin of native's
        ``FAUNA_ATPROTO_PLC_DIRECTORY_URL`` env var. Reaches
        ``window.__fauna_enableFakePlcDirectoryForTest`` (set up in
        ``apps/fauna-web/src/lib/e2e-automation.ts``), which loads the
        atproto-settings wasm chunk itself if it isn't up yet. Web-only;
        native sets the env var in its launch config instead.
        """
        self._ensure_agent()
        url_json = json_mod.dumps(url)
        self._execute_js(
            "(async () => { await window.__fauna_enableFakePlcDirectoryForTest("
            f"{url_json}); return null; }})()"
        )

    # --- E2E bridge contract: machine method dispatch ---
    #
    # Per the E2E bridge contract (tracked internally): every per-app
    # driver implements
    # call_machine_method(name, json_arg) so test code can fixture
    # OnboardingMachine state without driving the real probe path.
    # Web reaches the wasm OnboardingMachine via window.__fauna_callMachineMethod
    # set up in apps/fauna-web/src/lib/onboarding/machine.svelte.ts.
    def call_machine_method(self, name: str, json_arg: str = "") -> object:
        """Invoke an OnboardingMachine method by name, passing json_arg.

        json_arg is the JSON-encoded argument as a string (test setters
        accept the raw JSON; other methods get the parsed value). Returns
        whatever the wasm method returns, JSON-stringified across the
        bridge.
        """
        self._ensure_agent()
        # JSON-escape the args before embedding into the JS expression.
        arg_repr = json_mod.dumps(json_arg)
        name_repr = json_mod.dumps(name)
        # Wait for the call to settle — most methods are async; the
        # observer fires on completion which triggers re-render. Sleep a
        # short moment so subsequent assertions see the new snapshot.
        # The await IS necessary: setHandleCheckSnapshotForTest is sync
        # but returns through a Promise from the wasm-bindgen layer when
        # the agent does `await` on it.
        # Two doors, and which one is open depends on the route. The
        # machine-bound hook only exists once the onboarding page has been
        # mounted (it is that module's install); on a live authenticated session
        # — where the post-auth identity test seeds its pin — the page was never
        # mounted, so the machine-free bridge is the only one up. Prefer the
        # machine hook when present (it serves every name), and fall back rather
        # than failing on a route that simply has no machine.
        script = (
            "(async () => { "
            "const w = window; "
            "const r = typeof w.__fauna_callMachineMethod === 'function' "
            f"? await w.__fauna_callMachineMethod({name_repr}, {arg_repr}) "
            f": await w.__fauna_callMachineFreeMethod({name_repr}, {arg_repr}); "
            "return r === undefined ? null : r; "
            "})()"
        )
        result = self._execute_js(script)
        time.sleep(0.1)
        self._sync_state_to_cache()
        return result

    # --- E2E bridge contract: arbitrary command dispatch ---
    #
    # The web twin of the native bridges' `call_command` (HttpBridgeDriver).
    # Domain action layers (conversations) post `conversations_*` commands that
    # the running SPA routes to the shared wasm `ConversationsManager` via
    # `window.__fauna_callCommand` (installed in `$lib/conversations.ts`,
    # `installConversationsCommandHook`). Mirrors `call_machine_method`'s
    # app-side-global dispatch.

    def call_command(self, action: str, payload: dict | None = None, timeout: float = 5.0) -> str | None:
        """Route a bridge command to the SPA's `window.__fauna_callCommand`.

        `payload` is JSON-encoded and passed as a single string arg (the hook
        JSON-parses it). The call is awaited so the wasm mutation + snapshot
        refresh complete before the test reads `data.conversation_threads`.

        Returns the handler's own return value (``None`` when it produces none),
        matching the http-bridge driver — the hook's promise already resolved to
        it and this merely stops discarding it.
        """
        self._ensure_agent()
        payload_json = json_mod.dumps(json_mod.dumps(payload or {}))
        action_repr = json_mod.dumps(action)
        script = (
            "(async () => { "
            "if (typeof window.__fauna_callCommand !== 'function') "
            f"throw new Error('__fauna_callCommand not installed'); "
            f"const r = await window.__fauna_callCommand({action_repr}, {payload_json}); "
            "return r === undefined ? null : r; "
            "})()"
        )
        result = self._execute_js(script)
        # Push the refreshed snapshot to the bridge cache so a subsequent
        # HTTP get_state() (list_threads) sees the post-command state.
        self._sync_state_to_cache()
        return result

    def barrier(self, timeout: float = 10.0) -> None:
        """Convention 14's causal anchor on web.

        `call_command` above awaits the handler's promise, and `$lib/barrier-e2e`
        makes that promise resolve only after two macrotask turns — awaiting a
        promise alone would order this against microtasks only. The state sync
        `call_command` already does then republishes the post-barrier snapshot,
        so a following `get_state()` reads what the barrier waited for.
        """
        self.call_command("barrier", timeout=timeout)

    # --- Convention 17 layer (c): focus_move / switch_pane, driver-level ---
    #
    # Both doors are overridden here rather than left to the base class's
    # `call_command` default (`drivers/base.py`) because in-page JS has no
    # real key door: an untrusted `KeyboardEvent('keydown', {key: 'Tab'})`
    # dispatched from script moves no focus at all (measured 2026-08-21,
    # bundled Chromium). The browser's own sequential-focus navigation is
    # reachable only from the Playwright side — `page.keyboard.press` goes
    # through CDP as a TRUSTED key event — so these presses go straight to
    # the bridge's bare `/keyboard/press` route (never `/element/key`, which
    # focuses a target element first and would restart the tab order rather
    # than advance it). See `e2e-systematic-ui-walks.md` § Implementation
    # status today → the web-leg entry for the full design ruling; web's
    # registry keeps `focus_move`/`switch_pane` as named refusals pointing
    # back here (`$lib/focus-walk-e2e.ts`), so a caller that reaches them via
    # `call_command` is told where the real door is.

    def focus_move(self, direction: str = "next", times: int = 1) -> None:
        """Advance the keyboard focus `times` positions via real Tab presses.

        Payload rulings mirror `fauna_e2e_agent::focus_move_request`: a
        present-but-malformed `direction`/`times` raises naming the command
        (never silently coerced), and `times` above `_FOCUS_MOVE_MAX_TIMES`
        raises rather than clamps — a clamp would run a command nobody asked
        for. Overrides `PlatformDriver.focus_move` (`drivers/base.py`).
        """
        if direction not in ("next", "prev"):
            raise ValueError(
                f'focus_move: `direction` must be "next" or "prev", got {direction!r}'
            )
        if not isinstance(times, int) or isinstance(times, bool) or times < 0:
            raise ValueError(
                f"focus_move: `times` must be a non-negative integer, got {times!r}"
            )
        if times > _FOCUS_MOVE_MAX_TIMES:
            raise ValueError(
                f"focus_move: `times` is {times}, above the {_FOCUS_MOVE_MAX_TIMES} "
                "cap — the step loop runs on the thread that serves this agent, so "
                "a count that large stalls every later command rather than just "
                "this one"
            )
        key = "Tab" if direction == "next" else "Shift+Tab"
        for _ in range(times):
            self._post("/keyboard/press", {"key": key})
        self._sync_state_to_cache()

    # One shared probe script for `switch_pane`, run after every press (and
    # once, with `markStart` set, before the first). Regions resolve by
    # LANDMARK TYPE, never a hand-written path — the profile page mounts a
    # `<nav class="tabs">` INSIDE `<main>` (routes/profile/[[actorId]]/
    # +page.svelte), so `sidebar` must exclude any `<nav>` that has a `<main>`
    # ancestor. `window.__fauna_switch_pane_start` persists the starting
    # element's identity ACROSS separate `/execute` round trips (a JS local
    # can't survive them) so a later probe can tell whether the ring has
    # cycled all the way back without ever entering the target region.
    _SWITCH_PANE_PROBE_JS = """
    (arg) => {
      const { pane, markStart } = arg;
      if (markStart) { window.__fauna_switch_pane_start = document.activeElement; }
      const region = pane === 'page'
        ? document.querySelector('main:not([hidden])')
        : Array.from(document.querySelectorAll('nav')).find((n) => !n.closest('main'));
      if (!region) return { missing: true };
      const active = document.activeElement;
      return {
        missing: false,
        inside: region.contains(active),
        onBody: active === document.body,
        atStart: !markStart && active === window.__fauna_switch_pane_start,
      };
    }
    """

    def switch_pane(self, pane: str) -> None:
        """Hand the keyboard to the `page` or `sidebar` landmark via real
        Tab/Shift-Tab presses through the real door — DOM has no keystroke
        or API that jumps between landmarks, so this is what a keyboard user
        actually does: press through the ring until `document.activeElement`
        lies inside the target region.

        Four terminations (`e2e-systematic-ui-walks.md` § web leg): already
        inside → no-op; a full ring cycle back to the start WITHOUT ever
        visiting `BODY` (headless Chromium's own wrap discriminator) → no-op
        (the region genuinely has no tab stops in this fixture state); a
        cycle back to the start HAVING visited `BODY` → a focus trap, loud
        refusal; `_FOCUS_MOVE_MAX_TIMES` presses with no termination → loud
        refusal. Overrides `PlatformDriver.switch_pane` (`drivers/base.py`).
        """
        if pane not in ("page", "sidebar"):
            raise ValueError(f'switch_pane: `pane` must be "page" or "sidebar", got {pane!r}')
        self._ensure_agent()
        probe = self._execute_js(self._SWITCH_PANE_PROBE_JS, {"pane": pane, "markStart": True})
        if probe.get("missing"):
            landmark = "`<main>`" if pane == "page" else "`<nav>` outside `<main>`"
            raise RuntimeError(f"switch_pane: no {landmark} landmark found in the DOM")
        if probe["inside"]:
            self._sync_state_to_cache()
            return
        key = "Tab" if pane == "page" else "Shift+Tab"
        saw_body = bool(probe["onBody"])
        for presses in range(1, _FOCUS_MOVE_MAX_TIMES + 1):
            self._post("/keyboard/press", {"key": key})
            probe = self._execute_js(
                self._SWITCH_PANE_PROBE_JS, {"pane": pane, "markStart": False}
            )
            if probe.get("missing"):
                landmark = "`<main>`" if pane == "page" else "`<nav>` outside `<main>`"
                raise RuntimeError(
                    f"switch_pane: no {landmark} landmark found in the DOM "
                    "(disappeared mid-traversal)"
                )
            if probe["onBody"]:
                saw_body = True
            if probe["inside"]:
                self._sync_state_to_cache()
                return
            if probe["atStart"]:
                if saw_body:
                    self._sync_state_to_cache()
                    return  # the region has no tab stops — same ruling macOS recorded
                raise RuntimeError(
                    f"switch_pane: focus never left a cycle of {presses} elements "
                    f"that excludes {pane} — a focus trap"
                )
        raise RuntimeError(
            f"switch_pane: pressing {key} {_FOCUS_MOVE_MAX_TIMES} times never "
            f"reached the {pane} region"
        )

    # --- Test State Protocol (JS eval short-circuit) ---

    def set_state(self, state: dict, timeout: float = 10.0,
                  *, wait_ready: bool = True) -> None:
        """Apply state patch via JS agent (no HTTP polling needed for web).

        ``wait_ready`` is accepted for signature parity with the bridge drivers
        and ignored: there is no deferred-readiness flag here — the patch is a
        synchronous JS call, and the SPA paints its Privacy page immediately
        whether or not the mode has arrived.
        """
        self._ensure_agent()
        session = state.get("session") if isinstance(state, dict) else None
        if isinstance(session, dict) and session.get("secret_hex") and not session.get("actor_id"):
            # The agent writes the session straight into the account registry's
            # localStorage shape (`fauna/index` + `fauna/{actor}/…`), which is
            # keyed by the actor id the secret derives to — derive it here, in
            # Python, since the injected agent script has no wasm to hand.
            from common.accounts import actor_id_hex

            state = {**state, "session": {**session, "actor_id": actor_id_hex(session["secret_hex"])}}
        patch_json = json_mod.dumps(state)
        self._execute_js(f"window.__faunaTestAgent.applyPatch({patch_json})")
        # Wait for SvelteKit goto() and store updates to settle
        time.sleep(0.5)
        # Re-ensure agent in case navigation caused a full page reload
        self._ensure_agent()
        self._sync_state_to_cache()

    def get_state(self, path: str | None = None, *,
                  wait_for=None, timeout: float = 5.0) -> dict | None:
        """Read state via JS agent (synchronous — no polling needed).

        Web state is read via synchronous JS eval, so there's no stale-data
        window. The wait_for predicate is checked once; if it fails, the state
        is returned as-is (caller decides what to do).
        """
        self._ensure_agent()
        result = self._execute_js("JSON.stringify(window.__faunaTestAgent.getState())")
        if not result:
            return None
        state = json_mod.loads(result)
        if path:
            for key in path.split("."):
                if isinstance(state, dict):
                    state = state.get(key)
                else:
                    return None
        return state

    def reset(self, timeout: float = 10.0) -> None:
        """Reset app to factory state."""
        # A crash-recovery journey that failed after kill_uncleanly() (before its
        # relaunch) leaves page "1" closed; the session-scoped driver's next reset()
        # must reopen it rather than error on a dead page and cascade-fail the suite.
        if self._app_killed:
            self._fresh_boot()
        self._ensure_agent()
        # The reset action ends in a hard `location.href` navigation (agent.js —
        # wasm module reinit needs a real reload, not SvelteKit's client-side
        # goto), so it's exactly as heavy as `_fresh_boot`'s reload/relaunch and
        # shares the same generous ceiling (HEAVY_BOOT_TIMEOUT_S, above).
        self._execute_js(
            "window.__faunaTestAgent.applyPatch({__action: 'reset'})",
            timeout=HEAVY_BOOT_TIMEOUT_S,
        )
        time.sleep(0.3)
        # Re-inject agent in case goto() triggered a full reload
        self._ensure_agent()
        self._sync_state_to_cache()

    def logout(self, timeout: float = 10.0) -> None:
        """Clear session, keep data."""
        self._ensure_agent()
        self._execute_js("window.__faunaTestAgent.applyPatch({__action: 'logout'})")
        time.sleep(0.3)
        self._ensure_agent()
        self._sync_state_to_cache()

    def _sync_state_to_cache(self) -> None:
        """Push current JS agent state to the bridge cache for HTTP get_state()."""
        try:
            result = self._execute_js("JSON.stringify(window.__faunaTestAgent.getState())")
            if result:
                self._post("/app/state", {
                    "last_command_id": "js_eval",
                    "state": json_mod.loads(result),
                })
        except Exception:
            pass  # Agent may be unavailable mid-navigation
