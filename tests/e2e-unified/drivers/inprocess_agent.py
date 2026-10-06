from __future__ import annotations

import json
import os
import time
import urllib.request

from .http_bridge import HttpBridgeDriver


class RenderNotReady(RuntimeError):
    """The in-process app launched fully "healthy" but never rendered any UI.

    Raised by ``InProcessAgentDriver.assert_render_ready`` when the app process is
    alive and its automation server answers, yet no root view ever registers an
    element — see that method's docstring. It signals a degraded *host GUI* (an
    app that can't create a window), NOT an app code regression; the fix is
    environmental (reboot / GUI log-out-in).
    """


class InProcessAgentDriver(HttpBridgeDriver):
    """Shared skeleton for the apple in-process automation drivers (macOS + iOS).

    Both macOS and iOS drive the app over its in-app ``InProcessAutomationServer``
    (the unified ``/element/*`` + ``/app/*`` contract) bound to a per-instance
    ``FAUNA_E2E_AGENT_PORT`` — the XCUITest/AutomationMode replacement, the apple
    twin of linux's ``LinuxBridgeDriver``. NO xcodebuild, NO XCUITest, NO
    machine-wide AutomationMode → nothing to wedge (no reboots).

    The post-click settle, the ``/health`` probe, and ``recover()`` are identical
    across the two platforms and live here; only the *process model* differs and
    stays in the subclass: macOS spawns the ``FaunaMacOS`` binary directly (a
    `subprocess.Popen` it owns, with an isolated ``HOME``), iOS launches the app
    in the Simulator via ``simctl`` (terminated via ``simctl terminate``). The
    subclass implements ``launch(config)`` (setting ``self._launch_config`` and
    ``self._url``) and ``teardown()``.

    Authority + Phase-0/2/3 findings:
    docs/goal/architecture/apps/apple-e2e-automation.md.
    """

    # Maximum time to wait for a post-click child to appear when
    # `wait_for_child=` is supplied. Generous — most renders happen well under
    # 200ms; the ceiling is for the rare slow-machine case. (Mirrors linux.)
    _WAIT_FOR_CHILD_MAX_S = 1.5

    # Baseline post-click settle when no `wait_for_child=` is given. The server
    # actuates the registered closure synchronously on the main actor, but the
    # resulting SwiftUI re-render (and the `onAppear`-driven registration of any
    # newly-shown controls) is asynchronous, so a caller that immediately asserts
    # `is_visible(...)` can race it. Mirrors linux's empirical floor.
    _DEFAULT_POST_CLICK_SLEEP_S = 0.5

    #: Credential dir handed to the CURRENT app process (see
    #: `_resolve_credential_store`); read back by tests via
    #: `driver._cred_dir / "keychain.json"`.
    _cred_dir: str | None = None
    #: Set by `preserve_state_across_relaunch()`. When non-None, the next launch
    #: reuses it instead of minting a fresh one, so the relaunched app reads back
    #: the previous process's keychain.
    _preserved_cred_dir: str | None = None

    def _resolve_credential_store(self, config: dict, tmp: str) -> str:
        """The store half of `launch()`, identical on macOS and iOS: resolve this
        launch's credential dir, publish it, seed it, and return it.

        The app's E2E keychain is backed by `keychain.json` in this dir (FaunaKit
        `KeychainStore.e2eFileURL`). A FRESH dir per launch (`<tmp>/credentials`)
        is the default and is what gives each launch an empty store — the app
        skips its own wipe when this is set, because handing it a fresh dir
        already IS the clean start. `preserve_state_across_relaunch()` pins the dir
        instead, so the relaunched process reads back what this one wrote. Same
        contract as linux's credential dir. iOS can take a host path because the
        simulator shares the host filesystem (unlike a real device).

        `config["credential_dir"]` pins it explicitly, and outranks the relaunch
        pin: a caller naming the dir is stating which store this process must open,
        whereas the pin only records what the PREVIOUS launch of this driver
        happened to use.
        """
        cred_dir = (
            config.get("credential_dir")
            or self._preserved_cred_dir
            or os.path.join(tmp, "credentials")
        )
        os.makedirs(cred_dir, exist_ok=True)
        self._cred_dir = cred_dir
        # The store principal survives a relaunch (convention 10's carve-out,
        # `HttpBridgeDriver._begin_principal_slot_carry`): harvest the launch being
        # replaced BEFORE the `_resolved_*` attrs below move to this one. Every
        # launch gets a fresh dir, as on linux, and both apple `recover()`s are the
        # shared teardown + `launch()`, so this is where every relaunch lands. A dir
        # this launch did not mint is the caller's — a supplied `credential_dir`, or
        # the relaunch pin `preserve_state_across_relaunch()` set, whose store
        # already holds its slot — so it is never restored into.
        self._begin_principal_slot_carry(
            {"credential_dir": config.get("credential_dir") or self._preserved_cred_dir},
            {"FAUNA_E2E_CREDENTIAL_DIR": cred_dir},
        )
        # The generic cross-driver contract `_resolved_credential_dir` /
        # `_resolved_keyring_app` (linux/tui/windows) — some journey helpers poll
        # this pair via `getattr(driver, ..., None)` to wait for a durable on-disk
        # write before killing the process (e.g.
        # `_wait_for_own_pending_factory_reset_mint` in
        # test_crash_recovery_journeys.py). Apple's store is always the fixed
        # `keychain.json` under `_cred_dir` (`KeychainStore.e2eFileURL`), so
        # "keychain" is a constant, not a resolved value — but without these two
        # attributes such a wait silently no-ops (missing `getattr` default),
        # letting a SIGKILL race the async pre-dispatch mint
        # (`AdminNestVM.factoryReset()` awaits a live-nest `getAccount()` call
        # first, mirroring linux's `AccountClient::get()`) and lose the slot before
        # it lands.
        self._resolved_credential_dir = cred_dir
        self._resolved_keyring_app = "keychain"

        # Pre-seed the app's credential store, BEFORE the process starts
        # (linux/windows parity — `drivers/linux.py` / `drivers/windows.py` do the
        # same). This is what lets a test stand up a ≥2-account registry with **no
        # app code**: the flat logical-key map from `common.build_registry_seed` is
        # exactly the shape `KeychainSecretStore` reads (`fauna/index` +
        # `fauna/{actor}/…`, every key verbatim — apple retired its native
        # single-slot rows). `keychain.json` is the name
        # `KeychainStore.e2eFileURL` reads; the per-launch dir is what isolates
        # concurrent runs, so apple needs no `FAUNA_KEYRING_APP` namespace the way
        # the shared libsecret / Credential Manager stores do.
        cred_file = os.path.join(cred_dir, "keychain.json")
        seed = config.get("seed_credentials")
        if seed:
            with open(cred_file, "w") as f:
                json.dump(dict(seed), f)
            os.chmod(cred_file, 0o600)
        # The relaunch's SECOND survivor (convention 10, ruled 2026-09-20): the
        # install device secret the app derives its named sync-device-row id from.
        # apple keeps it as a row in this very file rather than a file under an
        # install dir, so its leg of the one carry is the row-shaped
        # `_begin_install_device_secret_row_carry` — LAST in this method, after
        # the seed, which replaces `keychain.json` wholesale. Unlike the file
        # leg, what it does HERE is only harvest: the secret is laid back down at
        # the first sign-in, beyond the `reset()` that follows every apple
        # relaunch and sweeps every row in the store (see that method's doc).
        self._begin_install_device_secret_row_carry(cred_file)
        return cred_dir

    def click(self, element_id: str, index: int = 0, *,
              scope: str | None = None,
              wait_for_child: str | None = None) -> None:
        """Click an element, then settle for the async post-click re-render.

        - `wait_for_child=<id>`: poll for that child to be visible with a
          `_WAIT_FOR_CHILD_MAX_S` ceiling. Use when the test knows which element
          should appear after the click.
        - default (no kwarg): briefly sleep `_DEFAULT_POST_CLICK_SLEEP_S` so the
          next assertion finds a settled tree.

        Either way, callers that need stronger guarantees should follow with an
        explicit `wait_for(...)` — the polling there is the authoritative wait.
        """
        super().click(element_id, index, scope=scope)
        if wait_for_child is not None:
            self.wait_for(wait_for_child, timeout=self._WAIT_FOR_CHILD_MAX_S)
            return
        time.sleep(self._DEFAULT_POST_CLICK_SLEEP_S)

    def in_viewport(self, element_id: str, *, index: int = 0,
                    scope: str | None = None) -> bool:
        """Found is in view here: the registry publishes only the slots SwiftUI
        has realized on screen (`AutomationRegistry.visibleSlots`), so a lazy
        list's rows outside the viewport are not counted at all and indices
        are on-screen order. A row the caller could not find is not in view."""
        return self.count(element_id, scope=scope) > index

    def get_clipboard_text(self) -> str | None:
        """The OS pasteboard's plain text, read in-process by the app's own
        ``GET /clipboard/text`` (``NSPasteboard`` on macOS, ``UIPasteboard`` in
        the iOS Simulator) — one implementation for both apple drivers, the
        windows / linux drivers' contract: ``None`` when the pasteboard holds no
        text; an agent refusal raises rather than reading empty.

        The pasteboard is a machine-wide resource (the iOS Simulator mirrors the
        host's by default), so a caller compares against a read taken BEFORE its
        copy rather than trusting the first non-empty value it sees."""
        reply = self._get("/clipboard/text")
        if "error" in reply:
            raise RuntimeError(f"apple pasteboard read refused: {reply['error']}")
        return reply.get("text")

    def _agent_health_ok(self) -> bool:
        try:
            with urllib.request.urlopen(f"{self._url}/health", timeout=1) as r:
                return r.status == 200
        except Exception:
            return False

    def seed_pending_factory_reset(
        self, nest_url: str, handle: str, claim_code: str
    ) -> None:
        """CR-2 slot seam (macOS + iOS). Both drive `KeychainSecretStore`, whose
        e2e backing is the flat `keychain.json` in this launch's `_cred_dir`
        (`KeychainStore.e2eFileURL`). The reconcile and apple's own launch both
        read the active account's per-actor registry record — apple reads no
        native row. See `PlatformDriver.seed_pending_factory_reset` for the
        contract."""
        self._ensure_pending_factory_reset_in_json_file(
            os.path.join(self._cred_dir, "keychain.json"),
            nest_url,
            handle,
            claim_code,
            native_mirror_prefix=None,
        )

    # Root control ids that ALWAYS register once the app's first view actually
    # renders. ``reset()`` returns the app to onboarding (the welcome screen), so
    # these are its entry controls (shared across macOS + iOS). If the app renders
    # at all, at least one appears within a second or two.
    _RENDER_PROBE_IDS = ("create-identity-button", "import-identity-button")

    def assert_render_ready(self, *, timeout: float = 12.0,
                            relaunch_attempts: int = 3) -> None:
        """One-time preflight: confirm the app actually *renders* its UI — not just
        that its automation server answers ``/health`` — **relaunching to self-heal
        a transient render-death before giving up.**

        The in-process app can launch fully "healthy" — process alive, ``/health``
        200, ``/app/state`` returning correct *logical* state — yet create **no
        window at all**. When that happens, ``NSApplication`` runs but its
        ``WindowGroup`` never materialises (confirmed via
        ``CGWindowListCopyWindowInfo`` showing zero windows for the process while it
        answers HTTP); with no window, no SwiftUI body evaluates, the
        ``AutomationRegistry`` stays empty, and EVERY element query times out —
        turning it into a ~45-minute all-red run that masquerades as an app
        regression (only the element-*reading* tests fail; the ``/app/state``-reading
        ones still pass, which is the tell).

        Render-death has two flavours (memory ``e2e-nest-cpu-contention-health-timeout``):
        a **transient per-launch** one that a fresh launch clears (apple-filesync saw
        first-launch-dead → next-launch-windowed, same session, no reboot), and a
        **total** one where *every* launch is render-dead even at low load (a
        full test run saw 8/8 dead, 2026-06-16 night) — that one is a degraded host GUI /
        WindowServer session needing an environmental fix (reboot / GUI log-out-in).

        So: probe; if render-dead, ``recover()`` (relaunch) and re-probe, up to
        ``relaunch_attempts`` times. Return as soon as a launch renders; only raise
        ``RenderNotReady`` if *every* (re)launch stays dead — i.e. the genuine
        machine-state case, where the message says "reboot", not "fix the code".
        Cheap on the happy path (onboarding renders in ~1-2s). (This is the
        render-coverage half of the ``pytest_sessionstart`` AutomationMode gate the
        XCUITest→in-process cutover removed without replacing — apple-e2e-automation.md.)
        """
        for attempt in range(relaunch_attempts + 1):
            if self._render_probe(timeout):
                return
            if attempt < relaunch_attempts:
                # Transient per-launch render-death clears on a fresh launch — try
                # one. (recover() = teardown + launch; returns False if it can't.)
                if not self.recover():
                    break
        raise RenderNotReady(
            f"in-process app launched healthy (/health ok, /app/state served) but "
            f"rendered NO UI across {relaunch_attempts + 1} launch attempt(s): none "
            f"of {self._RENDER_PROBE_IDS} ever registered (the automation registry is "
            f"empty because each app process is alive yet its window never "
            f"materialised). EVERY fresh launch render-dead ⇒ a degraded host GUI / "
            f"WindowServer session (an app that can't create a window), NOT an app "
            f"code regression (verified: /app/state stays correct while every rendered "
            f"element is absent). FIX: reboot or GUI log-out/in of the machine, then "
            f"re-run."
        )

    def _render_probe(self, timeout: float) -> bool:
        """Reset to onboarding, poll for a root control. True if one registers
        within ``timeout`` (the app rendered), False if none ever does (render-dead).
        """
        try:
            self.reset()
        except Exception:
            pass
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            for probe_id in self._RENDER_PROBE_IDS:
                try:
                    if self.count(probe_id) > 0:
                        return True
                except Exception:
                    pass
            time.sleep(0.5)
        return False

    def recover(self) -> bool:
        """Relaunch a wedged app so the next test gets a fresh instance.

        `teardown()` then `launch()` are both platform-specific (the subclass
        owns them); this orchestration is shared.
        """
        config = getattr(self, "_launch_config", None)
        if config is None:
            return False
        self.teardown()
        try:
            self.launch(config)
            return True
        except Exception:
            return False

    # hard_reload is inherited from HttpBridgeDriver (native relaunch via the
    # overridden recover() + replay of the last set_state session). macOS/iOS get
    # the byte-identical behaviour the former local override provided; windows/linux
    # gain it too. web keeps its own location.reload override.
