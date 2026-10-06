import json
from abc import ABC, abstractmethod
from pathlib import Path


def detached_agent_log_text(agent_data_dir: str | None) -> str:
    """The real `fauna-sync-agent`'s OWN daily-rolling log under
    ``agent_data_dir``, or ``""`` when there is none to read.

    The shared half of `app_stderr_text()` for every driver whose agent is
    spawned **DETACHED** rather than as an inherited-fd child. linux spawns the
    agent as a child, so its output lands in the same stderr the driver already
    captures and nothing extra is needed; on **windows** — for the windows app
    AND for the tui app running there, since the detachment is the OS's
    constraint, not the app's — the agent's output reaches the app's log never.
    `helpers/folder_content.py`'s `agent_diagnosis` / `await_agent_upload` read
    `app_stderr_text()` and nothing else, so without this they wait on a stream
    the evidence can never arrive in: the upload succeeds and the witness still
    fails, blind by construction.

    `bins/fauna-sync-agent`'s `run_main` writes `<data-dir>/logs/fauna.log.<date>`
    (`fauna_log::init`) — the same glob shape the app's own log uses.

    One definition on purpose: this was the windows driver's private body until
    2026-09-21, when the tui seat on windows needed the identical stitching and
    a second copy would have been the drift priority #4 forbids.
    """
    return rolling_log_text(agent_data_dir)


def rolling_log_text(data_dir: str | None) -> str:
    """Every `fauna_log` daily-rolling file under ``<data_dir>/logs/``, oldest
    first, or ``""`` — the on-disk shape shared Rust's `fauna_log::init` writes
    for every process that installs it (the agent, the windows app, tui)."""
    if not data_dir:
        return ""
    import glob
    import os

    chunks = []
    for path in sorted(glob.glob(os.path.join(data_dir, "logs", "*"))):
        try:
            with open(path, "r", encoding="utf-8", errors="replace") as fh:
                chunks.append(fh.read())
        except OSError:
            continue
    return "\n".join(chunks)


#: The line `stitch_agent_log` puts between the app's own log and the detached
#: agent's. `helpers/app_log_section.py::build` splits on it so each part keeps
#: its own tail — one tail over the joined text let the agent's pipe polling push
#: every line of the app out of a failure report (measured 2026-09-28, tui on Windows).
AGENT_LOG_BANNER = "===== detached fauna-sync-agent log (its own file) ====="


def stitch_agent_log(app_text: str, agent_text: str) -> str:
    """The app's own log followed by the detached agent's, split by
    :data:`AGENT_LOG_BANNER` — the one stitching every driver with a detached
    agent (windows, and tui on windows) returns from ``app_stderr_text()``."""
    if not agent_text:
        return app_text
    return f"{app_text}\n{AGENT_LOG_BANNER}\n{agent_text}" if app_text else (
        f"{AGENT_LOG_BANNER}\n{agent_text}"
    )


class PlatformDriver(ABC):
    """Abstract base for all platform-specific UI automation drivers.

    Each method operates on logical element IDs from ui.yaml.
    Drivers translate these to native selectors.
    """

    def is_mobile(self) -> bool:
        """True for Android and iOS. Use this instead of checking driver type."""
        return False

    def is_web(self) -> bool:
        """True for the Playwright web driver."""
        return False

    def is_ios(self) -> bool:
        """True for iOS. Use when iOS has a different flow from other mobile clients."""
        return False

    def is_android(self) -> bool:
        """True for Android. Use when Android has a different flow from other mobile clients."""
        return False

    def is_macos(self) -> bool:
        """True for macOS desktop app."""
        return False

    def is_linux(self) -> bool:
        """True for the Linux GTK desktop app."""
        return False

    def is_windows(self) -> bool:
        """True for the Windows WinUI 3 desktop app."""
        return False

    def is_tui(self) -> bool:
        """True for the fauna-tui terminal (ratatui) client."""
        return False

    def inherited_agent_log_text(self) -> str:
        """The sync agent's OWN rolling log, when the agent is an inherited-fd
        child whose lines are already interleaved in ``app_stderr_text()``.

        On POSIX (linux, tui) the agent shares the app's stderr, so its
        shared-crate lines (`fauna_client::ws_adapter`,
        `fauna_account_plane::account_driver`, …) are indistinguishable from
        the app's by target alone; its own `<config>/fauna/sync/logs` file is
        how a caller tells them apart. ``""`` where the agent is detached and
        stitched behind :data:`AGENT_LOG_BANNER` instead, or has no such file.
        """
        return ""

    def download_dir(self) -> str | None:
        """The directory the client saves e2e-mode downloads into, or None.

        Native save dialogs are not e2e-driveable, so a client's file-save
        affordances (e.g. `snapshot-file-download-button`) bypass the dialog
        under e2e and write into a deterministic per-platform directory the
        driver knows. Drivers whose client has wired the seam override this;
        the default None makes `BackupsActions.wait_for_downloaded_file` fail
        with a wiring hint instead of a silent timeout.

        Call it at each observation, never once before a wait: a driver whose
        app saves onto another filesystem (android's device) answers with a
        host mirror it refreshes on this call and at no other time.
        """
        return None

    def widget_dir(self) -> str | None:
        """Where the client writes its home-screen widget snapshot under e2e, or None.

        The widget itself runs outside the app's automation surface, so its
        witness reads the snapshot the app publishes for it from outside
        (`apps/common.md` § Home-screen widget). Only the apps whose widget
        reads such a snapshot override this; the default None makes
        `helpers.home_screen_widget` fail with a wiring hint.
        """
        return None

    @abstractmethod
    def launch(self, config: dict) -> None:
        """Launch the client app. Config keys vary by driver."""
        ...

    @abstractmethod
    def teardown(self) -> None:
        """Clean up driver resources and close the app."""
        ...

    def supports_unclean_kill(self) -> bool:
        """True when this driver can `kill_uncleanly()` its own app child."""
        return False

    def log_scope_across_relaunch(self) -> str:
        """What this family's app-log reader answers AFTER a cold relaunch.

        A DECLARED contract, not an accident of each driver's tmp policy — which
        is what it was until 2026-09-22, and the distinction is load-bearing.
        `helpers/real_rail_control.witness_real_rail` is a precondition of every
        real-conversations test on a launch-gate app, and it asks the log a
        yes/no question ("did the real FaunaMls rail start?"). If a reader can
        answer with a DEAD launch's words, that control is satisfiable by a
        ghost. Three families happen to close this by minting a fresh tmp dir in
        `launch()`; nothing said they had to, so a refactor could have taken it
        away silently. Now it is pinned per driver by
        `tests/test_module_relaunch.py`, and a change has to say so.

        Four answers, each a different verdict for a reader of the log:

        * ``"per-launch"`` — the reader cannot return a previous launch's bytes.
          tui, linux and macOS by construction (fresh `mkdtemp` + reopened
          `app.err` every `launch()`); iOS because `launch()` uninstalls the
          container, and — when a `preserve_state_across_relaunch()` pin keeps
          it — because `_mark_log_baseline()` takes a byte floor first.
        * ``"cumulative"`` — it CAN: windows' data-dir log is append-shared
          across relaunches (`drivers/windows.py::_app_log_since`, which exists
          precisely to slice from a mark). A test reading it as "this launch"
          must take a mark of its own.
        * ``"evicting"`` — a bounded ring (web's 500-entry console log), where
          a line's ABSENCE is never evidence it was not logged, whatever the
          launch. Sound for diagnosis, unsound as a control's negative half.
        * ``"none"`` — no reader at all (android), so there is nothing to scope.

        The default is ``"unknown"`` on purpose: a new driver must DECLARE, and
        until it does, the pin fails rather than a hand-kept table answering
        confidently for a family it never heard of (the `app_capabilities.py`
        lesson convention 7 records).
        """
        return "unknown"

    def kill_uncleanly(self) -> None:
        """SIGKILL the driver's OWN spawned app child — no SIGTERM, no cleanup
        handlers, no graceful teardown — simulating a client crash / power loss
        mid-operation (nest/common.md § Client-state recoverability).

        Contract: only ever signals the process handle this driver itself
        spawned (never name-based — process safety, e2e-unified/README.md).
        After the kill the app is GONE; don't drive any element method until a
        relaunch (`hard_reload()`, which tolerates the dead child and replays
        the injected session, exactly what a crash-relaunch needs).

        Concrete with a NotImplementedError default: implemented where the
        driver owns its app child as a Popen (linux/macOS/tui via
        HttpBridgeDriver); windows asks the FlaUI bridge that owns the app to
        kill it (`WindowsDriver.kill_uncleanly`). web (the "app" is a browser
        page), ios (simctl owns it) and android need their own shapes — don't
        fake them with a graceful quit.
        """
        raise NotImplementedError(
            f"{type(self).__name__} has no unclean-kill primitive yet — the "
            f"driver must SIGKILL its OWN spawned app child, and this platform's "
            f"app process is not a driver-owned Popen"
        )

    #: How far in the past the seam stamps a seeded pending-factory-reset
    #: record's `minted_at_secs`: twice the launch machine's mint grace
    #: (`FACTORY_RESET_CLAIM_GRACE_SECS` = 15 min, fauna-launch-machine
    #: persistence.rs), so the reconcile's `Claimed` arm reads the slot as stale
    #: with a wide margin. The web bridge's seed (web-bridge/server.py) uses the
    #: same age.
    STALE_FACTORY_RESET_AGE_SECS = 2 * 15 * 60

    def seed_pending_factory_reset(
        self, nest_url: str, handle: str, claim_code: str
    ) -> None:
        """Ensure a STALE pending-factory-reset slot (CR-2) is present in this
        client's durable long-term store, written in the platform's REAL store
        format so a relaunch exercises the boot reconcile against a GENUINE slot —
        not a shim the store never reads back (that would pass vacuously, a false
        green worse than a red; nest/common.md § Client-state recoverability).

        STALE is the contract (2026-07-17): the seeded record carries a
        `minted_at_secs` [`STALE_FACTORY_RESET_AGE_SECS`] in the past — twice the
        machine's 15-minute mint grace (`FACTORY_RESET_CLAIM_GRACE_SECS`,
        launch-machine persistence.rs), so the `Claimed` arm reads it as stale and
        clears it. The field is required (a record without it does not parse, and
        an unparseable slot reads as NO slot — a vacuous seed), so the seam ages
        the stamp rather than dropping it. The seed OVERWRITES any fresh slot the
        client's own mint left before the kill — the overwrite is the seam's
        time-compression: a seconds-old mint is (correctly, by design) HONORED by
        the grace arm, which is the wrong scenario for a staleness test.

        ⚠ **The per-actor registry slot is the only slot the reconcile reads**
        (2026-08-28, measured, and since 2026-09-24 the pre-registry global
        triple is not read anywhere at all): the launch adapter reads
        `fauna/<actor_id>/pending_factory_reset` (a JSON record,
        `minted_at_secs` included) for the active account. A seam that wrote
        anything else would write into a slot the reconcile never reads — which
        does not fail loudly, it makes the journey VACUOUS: no slot means no
        trap, and "landed on the feed" then proves nothing about the reconcile.
        The implementation below therefore stales the per-actor record when the
        client wrote one, and writes one for the active account when it did not.

        Concrete with a NotImplementedError default (the `kill_uncleanly` pattern):
        implemented by every driver whose durable store the harness can both SEED
        and PRESERVE across a relaunch — the linux File backend and the
        macOS/iOS/Windows keychain files. A driver that `supports_unclean_kill()`
        MUST implement it rather than let journey 6 skip: a silent skip on a
        killable driver hides the very CR-2 gap the journey exists to prove
        (testing.md § point 11)."""
        raise NotImplementedError(
            f"{type(self).__name__} does not implement seed_pending_factory_reset()"
        )

    @staticmethod
    def _ensure_pending_factory_reset_in_json_file(
        path: str,
        nest_url: str,
        handle: str,
        claim_code: str,
        *,
        native_mirror_prefix: str | None,
    ) -> None:
        """Shared read-modify-write for the JSON-file-backed credential stores (the
        native keychain files + the linux/tui File backend): leave a STALE
        pending-factory-reset slot in the shape the reconcile reads.

        The authoritative shape is the **per-actor** registry slot
        `fauna/<actor_id>/pending_factory_reset` — one JSON record,
        `minted_at_secs` included — which the launch adapter reads for the
        active account (`RegistryLaunchPersistence::load_pending_factory_reset`;
        the pre-registry global slot is no longer read anywhere, 2026-09-24).
        A record the client itself wrote is REWRITTEN here with its
        `minted_at_secs` moved [`STALE_FACTORY_RESET_AGE_SECS`] into the past,
        which is exactly the ageing the seam exists to do: the
        client's own mint is seconds old and the grace arm would (correctly)
        honour it. When the client left no record (its mint never landed), one
        is written for the index's active account from the caller's arguments,
        so the journey still meets a slot rather than passing vacuously.

        `native_mirror_prefix`: a native app that still reads the slot back
        through its native single-slot rows — a mirror the boot re-mirror
        rewrites from the per-actor slot (android, until its leg of the
        single-slot retirement lands) — passes the prefix those rows carry so
        the mirror is pre-populated for the relaunch's own reads too. Every app
        that reads the registry alone (linux, tui, apple, windows) passes
        `None`."""
        import json
        import os
        import time

        stale_minted_at = int(time.time()) - PlatformDriver.STALE_FACTORY_RESET_AGE_SECS
        store: dict = {}
        if os.path.exists(path):
            with open(path) as f:
                store = json.load(f)

        if native_mirror_prefix is not None:
            store[f"{native_mirror_prefix}pending_factory_reset_nest_url"] = nest_url
            store[f"{native_mirror_prefix}pending_factory_reset_handle"] = handle
            store[f"{native_mirror_prefix}pending_factory_reset_claim_code"] = claim_code

        # The authoritative shape, aged rather than invented where the client
        # wrote one: only the timestamp is touched, so what the reconcile reads
        # back is a slot this client genuinely wrote, aged. Matching on the key
        # SUFFIX keeps the actor id out of this seam's signature — the registry
        # owns that spelling.
        aged = 0
        for key in [k for k in store if k.endswith("/pending_factory_reset")]:
            try:
                record = json.loads(store[key])
            except (TypeError, ValueError):
                continue  # not the registry's JSON record; leave it alone
            record["minted_at_secs"] = stale_minted_at
            store[key] = json.dumps(record)
            aged += 1
        if aged == 0:
            try:
                active = json.loads(store.get("fauna/index", "null") or "null")["active"]
            except (TypeError, ValueError, KeyError):
                active = None
            if active:
                store[f"fauna/{active}/pending_factory_reset"] = json.dumps(
                    {
                        "nest_url": nest_url,
                        "handle": handle,
                        "claim_code": claim_code,
                        "minted_at_secs": stale_minted_at,
                    }
                )

        with open(path, "w") as f:
            json.dump(store, f)
        os.chmod(path, 0o600)

    @abstractmethod
    def find_element(self, element_id: str, index: int = 0, *, scope: str | None = None):
        """Find element by logical ID. Returns native element handle."""
        ...

    @abstractmethod
    def click(self, element_id: str, index: int = 0, *, scope: str | None = None) -> None:
        """Click/tap the element."""
        ...

    def double_click(self, element_id: str, index: int = 0, *, scope: str | None = None) -> None:
        """Double-click the element.

        Concrete (not abstract) with a NotImplementedError default: only the
        drivers that need it override it (web Playwright `dblclick`; the FlaUI
        bridge). Used for the Outlook day-cell double-click → new-event compose
        (events.md § Layout & flow).
        """
        raise NotImplementedError(f"{type(self).__name__} does not implement double_click()")

    def scroll_to(self, element_id: str, index: int = 0, *, scope: str | None = None) -> None:
        """Targeted scroll: bring the (indexed, scoped) element into its scroll
        container's viewport, driving the client's REAL scroll position (the
        engagement-cue observer samples that same position, so a dwell driven
        through this is an honest exposure).

        Concrete with a NotImplementedError default (the double_click pattern):
        bridges serving POST /element/scroll-into-view get the HttpBridgeDriver
        implementation; anything else fails loudly rather than silently
        not scrolling.
        """
        raise NotImplementedError(f"{type(self).__name__} does not implement scroll_to()")

    def is_visible_scrolled(self, element_id: str, index: int = 0, *,
                            scope: str | None = None) -> bool:
        """``is_visible``, first bringing the element into its scroll container's
        viewport — the honest check for anything that can sit **below the fold**
        of a scrolling container (a long nav pane, a long settings page).

        Why this exists: a client may report a rendered-but-below-the-fold element
        as not visible (windows' ``is_visible`` reads UIA ``IsOffscreen``, and an
        unarranged row has a 0x0 rect), so a bare ``is_visible`` conflates *"the
        app never rendered it"* with *"it is one scroll away"*. Those have
        completely different causes, and confusing them cost three sessions on the
        gated `admin-tab`/`family-tab` rows, which are simply the last entries in a
        nav pane taller than the window.

        Cross-app by construction: the scroll is best-effort, so a driver with
        no scroll-into-view support (``NotImplementedError``) or a client where the
        element is already in view degrades to a plain ``is_visible``. It never
        masks a genuinely absent element — that still returns False.

        ⚠ Read that as a POSITIVE-read form. On a genuinely ABSENT element the
        scroll is not free everywhere: web's ``scroll_to`` is Playwright's
        ``scroll_into_view_if_needed``, which waits for the locator to *appear* and
        so runs the whole bridge timeout before failing
        (``WebBridgeDriver.is_nav_tab_revealed`` records the 500). A NEGATIVE read
        therefore uses :meth:`is_absent`, never ``not is_visible_scrolled(...)``,
        in any test that runs on more than windows.
        """
        try:
            self.scroll_to(element_id, index, scope=scope)
        except (NotImplementedError, LookupError, TimeoutError):
            # No scroll support, or nothing to scroll to — fall through to the
            # plain visibility read, which reports the real answer either way.
            pass
        return self.is_visible(element_id, scope=scope)

    def is_absent(self, element_id: str, *, scope: str | None = None) -> bool:
        """The honest form of a NEGATIVE visibility read: the app has NOT put this
        element in its tree, whatever the scroll position — ``assert
        driver.is_absent(x)`` where ``assert not driver.is_visible(x)`` would be
        vacuous on a scrollable surface (e2e-conventions.md convention 6's rider).

        Why it is a driver method rather than ``count(x) == 0`` in the test: the
        bridges disagree about what ``is_visible`` means, and only windows' carries
        a VIEWPORT predicate (``!IsOffscreen``), so a defect that really painted
        ``x`` below the fold reads "not visible" there and the negative assertion
        passes against the very bug it exists to catch. Everywhere else the
        default below is already exact, and it is deliberately NOT ``count == 0``:
        web's ``/element/count`` is ``locator.count()``, which counts a
        hidden-in-DOM element, so a bare count would red a correct web build.

        The per-app difference therefore lives here, in the driver shell, and a
        test states only the intent. An app whose ``/element/visible`` turns out to
        carry a viewport predicate overrides this the way ``WindowsBridgeDriver``
        does (``test_driver_is_absent.py`` pins who overrides).

        The default is ``not is_visible`` — no scroll and no ``count`` — so it
        changes nothing for any app that adopts it.
        """
        return not self.is_visible(element_id, scope=scope)

    def is_nav_tab_revealed(self, element_id: str, *, scope: str | None = None) -> bool:
        """Whether a *gated navigation tab* (``admin-tab`` / ``family-tab`` — a nav
        entry the shell reveals only when a gate fires) is REVEALED.

        The honest signal for a gated nav tab is tree-membership, not pixel-
        visibility: it is a fixed nav row the shell shows or hides, so "revealed"
        means "the app put it in the nav", full stop — whether it then happens to
        land below the fold of a nav pane taller than the window is an irrelevant
        window-size artifact. Reading pixel-visibility would force a scroll, and on
        some clients (windows/FlaUI) a UIA scroll of the nav pane's container can
        HANG the automation bridge (a standing gotcha). So a client whose gated
        tabs are removed-from-tree-until-revealed overrides this with a hang-free
        tree-membership check; the default keeps the cross-app behaviour
        (best-effort scroll + ``is_visible``) for clients where the tab is a
        persistent, always-in-tree sidebar entry toggled only by visibility.
        """
        return self.is_visible_scrolled(element_id, scope=scope)

    @abstractmethod
    def type_text(self, element_id: str, text: str, *, scope: str | None = None) -> None:
        """Type text into the element (appends to existing content)."""
        ...

    @abstractmethod
    def clear_and_type(self, element_id: str, text: str, *, scope: str | None = None) -> None:
        """Clear existing content and type new text."""
        ...

    def fill(self, element_id: str, text: str, *, scope: str | None = None) -> None:
        """Alias for ``clear_and_type``. Tests written in the
        Playwright/Selenium idiom call ``driver.fill(...)``; rather than
        force every test author to learn the slightly less obvious
        ``clear_and_type`` name, expose ``fill`` here on the abstract
        base. Default delegates so subclasses don't need to re-implement.
        """
        self.clear_and_type(element_id, text, scope=scope)

    @abstractmethod
    def get_text(self, element_id: str, index: int = 0, *, scope: str | None = None) -> str:
        """Get the text content of the element."""
        ...

    @abstractmethod
    def is_visible(self, element_id: str, *, scope: str | None = None) -> bool:
        """Check if the element is currently visible."""
        ...

    @abstractmethod
    def press_key(self, element_id: str, key: str, *, scope: str | None = None) -> None:
        """Focus the element and press a single named key (e.g. ``"Enter"``,
        ``"Escape"``, ``"Tab"``). Used by actions that need to commit input
        (a chip pick, a rename) where the UI listens for KeyDown rather
        than for an explicit confirm-button click. Key names follow the
        web ``KeyboardEvent.key`` convention.
        """
        ...

    @abstractmethod
    def get_attr(
        self, element_id: str, attribute: str, index: int = 0, *, scope: str | None = None
    ) -> str | None:
        """Read a named attribute of the element. Returns None if the
        attribute isn't set or doesn't apply.

        ``index`` picks among matches, exactly as it does for ``get_text`` /
        ``click`` — required for an attribute on an *indexed* element (e.g. the
        ``selected`` flag on ``dm-message-timestamp``).

        Per-platform mapping:
          * Web: ``element.getAttribute(name)`` (data-* attrs match without
            the data- prefix as a convenience).
          * WinUI: ``"disabled"`` -> ``"true"`` / ``"false"`` based on
            ``!IsEnabled``; ``"checked"`` -> HelpText if published, else the
            TogglePattern state; other names map to ``AutomationProperties.HelpText``.
            (UIA exposes a single application-supplied string per element;
            tests that need multiple attributes per element should pack
            them e.g. ``"state=resolved;rail=fauna"`` and parse on read.)
          * tui / linux / apple: served by the shared ``GET /element/attr``
            route (`libs/fauna-e2e-agent`, `apps/fauna-linux/src/automation`,
            FaunaKit's `InProcessAutomationServer`).
          * Android: the androidTest bridge (``BridgeHttpServer``/``ElementOps``)
            serves ``GET /element/attr`` since 2026-09-26 over the raw
            accessibility tree: a name answers the element's Compose
            ``stateDescription`` (the app's one string-attribute carrier —
            ``state``, ``class``, ``role``, ``copied``, …), ``state``/``checked``
            fall back to an unmarked toggle's native checked state as
            ``"true"``/``"false"``, ``enabled``/``disabled``/``text`` read the
            node. Compile-verified only until android has a run venue
            ; before that date the
            route did not exist and every read answered ``None``.

        Common attribute names tests use today:
          * ``"disabled"`` (capability gating) — ``"true"`` / ``"false"``
          * ``"state"`` (recipient-resolve-status) — one of
            ``"idle"`` / ``"resolving"`` / ``"resolved"`` / ``"error"``.
          * ``"checked"`` (any checkbox / toggle) — ``"true"`` / ``"false"``:
            tui publishes it per checkbox, linux reads the live toggle, web
            answers off the element's ``data-checked``, windows reads the
            control's TogglePattern unless the app publishes HelpText, apple
            maps a toggle's registered ``"on"``/``"off"`` value.
          * ``"selected"`` (``dm-message-timestamp``) — ``"true"`` / ``"false"``,
            the selected-message marker (`conversations.md` § The selected
            message).
          * ``"state"`` (``post-image``) — ``"painted"`` once decoded bytes are
            the picture, ``"placeholder"`` while they are not: the headless
            witness of fetch → open → decode → paint (`ui/media.md`
            § Encryption at rest). linux answers it off the live
            ``gtk::Picture``; web has no attribute for it and reads the
            ``<img>``'s ``naturalWidth`` instead
            (``FeedActions._post_image_painted_in``).
          * ``"state"`` (``video-thumbnail``) — the inline player's phase:
            ``"idle"`` (glyph, nothing played — never autoplay), ``"loading"``
            (tapped; the shared ``playback_source`` is resolving or the player is
            buffering), ``"playing"`` (the native player fired ``playing``),
            ``"error"`` (nothing playable, or the player's own error)
            (`render-model.md` § D6c → *Inline playback*). web publishes it as
            ``data-state``, macos + ios through the registry (FaunaKit's
            ``VideoThumbnailView``); the other apps add it as their player leg
            lands.
          * ``"position"`` / ``"source"`` (``video-thumbnail``) — the native
            player's position in seconds (``"0.000"`` while there is none) and
            the URL it was handed (``""`` while there is none): what web reads
            off its ``<video>``'s ``currentTime`` / ``currentSrc``, published as
            attributes by the apps whose player no script can reach (macos +
            ios today).
        """
        ...

    @abstractmethod
    def wait_for(self, element_id: str, timeout: float = 10.0, *, scope: str | None = None) -> None:
        """Wait until the element is visible. Raises TimeoutError."""
        ...

    @abstractmethod
    def count(self, element_id: str, *, scope: str | None = None) -> int:
        """Count how many instances of this element exist."""
        ...

    def get_texts(self, element_id: str, *, scope: str | None = None) -> list[str]:
        """Every ``element_id``'s text, in the same occurrence order ``index``
        addresses them in — the BULK twin of :meth:`get_text`.

        Use it wherever a helper wants the whole column (``[get_text(id, i)
        for i in range(count(id))]``): that loop is 1 + N driver calls, and on
        a native bridge EVERY one of them runs its own whole-tree find before
        reading a single property, so the cost is O(N) calls x O(N) walk. One
        bulk call finds once and reads N times. Measured on windows against a
        61-bubble conversations thread: ~1.25 s per per-element call, ~390 s of
        a 403 s test spent inside two such loops.

        The default below IS that loop, so a driver whose bridge serves no bulk
        route needs no override and answers exactly what it always did — the
        seam is a speed-up, never a behaviour change. Overriding drivers must
        return the identical list, including its length: callers index into it
        with the same index they would pass to :meth:`get_text`, so a bulk read
        that skips or reorders an element silently re-points every later
        ``click(id, index=i)`` at the wrong row.

        Zero matches is ``[]`` (what ``range(count())`` gave), never an error —
        unlike :meth:`registry_snapshot`, there is no ``None`` "no such
        surface" answer here, because the default always works.
        """
        return [
            self.get_text(element_id, index=i, scope=scope)
            for i in range(self.count(element_id, scope=scope))
        ]

    def get_attrs(
        self, element_id: str, attribute: str, *, scope: str | None = None
    ) -> list[str | None]:
        """``attribute`` on every ``element_id``, in occurrence order — the
        BULK twin of :meth:`get_attr`. See :meth:`get_texts` for why the seam
        exists and what an override owes its callers.

        ``None`` entries mean what they mean for :meth:`get_attr` — the
        attribute isn't set or doesn't apply on that element — and must survive
        the bulk read rather than collapsing to ``""``, which would read as
        "set, but empty".
        """
        return [
            self.get_attr(element_id, attribute, index=i, scope=scope)
            for i in range(self.count(element_id, scope=scope))
        ]

    @abstractmethod
    def set_input_files(self, element_id: str, files: str | list[str]) -> None:
        """Attach file(s) to a file input element."""
        ...

    def registry_snapshot(self) -> list[dict] | None:
        """Every element the app currently publishes, as records — or ``None``
        where this app has no such endpoint.

        The *structured* counterpart of :meth:`tree`. ``tree`` answers "why did
        THIS lookup resolve absent" and is read by a human; this answers "what is
        on screen right now", and is read by a checker that wants to quantify
        over the whole frame instead of over hand-picked ids (e2e conventions
        point 17 — assert general invariants, not only hand-picked outcomes).

        Each record carries at least ``id``, ``index`` (the same occurrence index
        the driver addresses that element with, so a finding can be re-driven
        verbatim), ``enabled``, ``declares_enabled`` (an ``enabled`` that was
        *declared* versus one that merely *defaulted*), ``actuable`` (the driver
        can click it), ``editable`` (the driver can type into it) and ``scope``.

        ⚠ ``None`` and ``[]`` are OPPOSITE answers and callers must not collapse
        them: ``None`` is "this app publishes no such surface" — an ``n/a``, and
        never a finding — while ``[]`` is "this app looked and there is nothing
        registered", which on a page that should hold controls is a real one.
        Collapsing them is how a checker reports a clean sweep of a surface it
        never read (the failure mode `frame_invariants._ABSENT` exists for).
        """
        return None

    def diagnose(self, element_id: str, *, attrs: tuple[str, ...] = (),
                 scope: str | None = None) -> str:
        """Build a compact self-diagnosing snapshot of ``element_id`` for a
        wait/poll that just timed out (e2e rule 6 — *failures must diagnose
        themselves*).

        Returns a ``[id: visible=…, count=…, text=…, attr=…]`` string a caller
        folds into its ``TimeoutError``/``AssertionError`` message so the
        failure classifies itself — never-rendered (``visible=False``) vs.
        rendered-but-wrong-value (a ``count``/``text``/attr is present but not
        what was awaited) vs. attribute-not-surfaced-on-this-platform (an attr
        probe reports ``NotImplementedError``) — instead of forcing a re-run
        with a debugger.

        EVERY probe is individually guarded: a missing bridge endpoint, an
        absent element, or a platform that doesn't implement an accessor is
        reported in place and never masks the real timeout (the gold-standard
        precedent is ``ConversationsActions._wait_resolve``). Pass ``attrs`` to
        also read named ``get_attr`` values (e.g. ``("state",)`` for a
        resolve-status element). Safe to call only on the failure path of a
        wait loop: it performs a few extra bridge round-trips, immaterial when
        we are already failing.
        """
        parts: list[str] = []

        def probe(label: str, fn) -> None:
            try:
                parts.append(f"{label}={fn()!r}")
            except Exception as e:  # noqa: BLE001 — diagnostic only; never mask the timeout
                parts.append(f"{label}=<{type(e).__name__}: {e}>")

        probe("visible", lambda: self.is_visible(element_id, scope=scope))
        probe("count", lambda: self.count(element_id, scope=scope))
        probe("text", lambda: self.get_text(element_id, scope=scope)[:80])
        for a in attrs:
            probe(a, lambda a=a: self.get_attr(element_id, a, scope=scope))
        return f"[{element_id}: " + ", ".join(parts) + "]"

    def in_viewport(self, element_id: str, *, index: int = 0,
                    scope: str | None = None) -> bool:
        """Whether the ``index``-th ``element_id`` sits inside the visible band
        of the scroll container that holds it — the observable for "brought
        into view" (`conversations.md` § The selected message).

        Distinct from :meth:`is_visible`, which on most drivers answers
        "rendered and shown", true for a row scrolled far out of view. Only a
        driver that can answer the geometry question overrides this; the
        default raises, so a test built on it fails loudly on an app that
        cannot say rather than reading every row as in view.
        """
        raise NotImplementedError(f"{type(self).__name__} does not implement in_viewport()")

    def assert_on_screen(self, element_id: str, *, scope: str | None = None) -> None:
        """Assert `element_id`'s live frame (`get_attr(..., "frame")`,
        window-space "x,y,w,h") places it on screen: realized, non-zero size,
        non-negative origin.

        Catches the "geo-parked off-screen" class of bug that `is_visible()`
        alone misses — a layout overflow can shove an element's frame to a
        negative origin while it stays registered and individually-hittable-
        by-id (the documented incident: a ~467pt controlsBar overflow that
        geo-parked media rows at x=-36). Raises whatever `get_attr` itself
        raises on a platform without the frame endpoint — same guard callers
        already need around any other `get_attr(..., "frame")` use.
        """
        raw = self.get_attr(element_id, "frame", scope=scope)
        if not raw:
            raise AssertionError(
                f"'{element_id}' has no live frame (not realized / sentinel "
                f"never attached); {self.diagnose(element_id, attrs=('frame',), scope=scope)}"
            )
        try:
            x, y, w, h = (float(p) for p in raw.split(","))
        except ValueError:
            raise AssertionError(
                f"'{element_id}' frame {raw!r} did not parse as 'x,y,w,h'"
            ) from None
        if w <= 0 or h <= 0:
            raise AssertionError(
                f"'{element_id}' frame {raw!r} has zero/negative size — not "
                f"actually rendered; {self.diagnose(element_id, attrs=('frame',), scope=scope)}"
            )
        if x < 0 or y < 0:
            raise AssertionError(
                f"'{element_id}' frame {raw!r} is geo-parked off-screen "
                f"(negative origin); {self.diagnose(element_id, attrs=('frame',), scope=scope)}"
            )

    def select(self, element_id: str, value: str, *, index: int = 0,
               scope: str | None = None) -> None:
        """Select an option in a dropdown/select element by value.

        ``index`` disambiguates per-row pickers (e.g. an ``admin-users-tier-select``
        in each ``user-row``), mirroring ``click``/``get_text``. Default
        implementation raises NotImplementedError. Drivers for platforms with
        native dropdown widgets should override.
        """
        raise NotImplementedError(f"{type(self).__name__} does not implement select()")

    def option_texts(
        self, element_id: str, index: int = 0, *, scope: str | None = None
    ) -> list[str] | None:
        """The full list of option texts a picker (``element_id``) currently
        paints — not just the selected one.

        Selecting a value and reading it back can't tell two options with the
        SAME text apart (the read-back is just the text you selected by), so
        an injectivity assertion — no two options render identically — needs
        the whole painted set. Built on ``get_attr(element_id, "options")``,
        which a supporting driver encodes as a JSON array of option texts
        (the same membership set ``select``'s twin-rule refusal checks,
        e2e-conventions.md convention 11).

        Returns ``None`` when the driver/element doesn't implement the
        ``"options"`` attr — not every bridge does yet. An empty list is a
        real answer: the picker painted zero options.
        """
        raw = self.get_attr(element_id, "options", index=index, scope=scope)
        if raw is None:
            return None
        return json.loads(raw)

    def enable_dns_fake_provider(self) -> None:
        """Enable the e2e fake DNS provider (the ``fake-dns-ok:<zone>`` sentinel
        decorator) so a credentialed ``DnsManagementMachine`` can verify/publish
        offline — the managed-publish success path tier_3 can't reach (no real
        registrar in the sandbox).

        Default is a **no-op**: the native apps (linux/windows) enable the
        decorator through the ``FAUNA_DNS_PROVIDER_FAKE`` env var set in their
        launch config (read by the in-process Rust
        ``build_dns_management_machine_with_credentials``), so the call is
        already in effect by launch. Only the web driver — which has no process
        env path — overrides this to flip the wasm flag
        (``window.__fauna_enableDnsFakeProviderForTest``). Calling it
        unconditionally lets the cross-app managed-publish test stay free of
        per-platform branching (platform checks belong in the driver layer, not
        the test).
        """
        return None

    def enable_fake_plc_directory(self, url: str) -> None:
        """Point the ATProto genesis-seniority custody check
        (``critical-alerts.md`` feeder #1) at a fake PLC directory ``url`` so
        ``test_atproto_custody_alarm.py`` can mint/tamper/verify against
        ``FakePlcDirectory`` with no real ``plc.directory`` reachable.

        Default is a **no-op**: every native app reads
        ``FAUNA_ATPROTO_PLC_DIRECTORY_URL`` from its process environment
        (already set in the launch config, same shape as
        ``enable_dns_fake_provider`` above), so the override is already in
        effect by launch. Only the web driver — which has no process env path
        and whose custody check lives in a lazily-loaded wasm chunk — overrides
        this to flip the wasm flag
        (``window.__fauna_enableFakePlcDirectoryForTest``). Calling it
        unconditionally keeps the cross-app custody-alarm test free of
        per-platform branching.
        """
        return None

    def live_app_instance_count(self) -> int | None:
        """How many live app processes own THIS driver's isolated state, or
        ``None`` when the driver cannot ask the OS that question.

        The contract every driver's relaunch implicitly claims is "one app
        process at a time"; nothing ever checked it, and a second live instance
        is invisible from inside the app — it manifests only as the *next*
        launch behaving oddly. On windows that cost four sessions: a leaked instance kept the account's
        instance lock, so every relaunch after it landed in the launch-collision
        chooser, never started its TestAgent, and turned the rest of the session
        into "environmental" skips — coverage loss that reads like success.

        Windows answers it because its ``recover()`` is the one that deliberately
        REUSES the data + credential dirs, which is what makes a survivor
        harmful there; its bridge matches processes on ``FAUNA_E2E_DATA_DIR``
        (``flaui-bridge/ProcessScan.cs``), a per-driver ``mkdtemp``, so the
        count covers this session's own app and can never see a parallel
        session's. ``None`` elsewhere, so a caller writes
        ``if (n := driver.live_app_instance_count()) is not None: assert n == 1``
        and the assertion strengthens for free as other drivers implement it.
        """
        return None

    def get_clipboard_text(self) -> str | None:
        """Read the OS clipboard's text content.

        Concrete with a NotImplementedError default (the `double_click` /
        `scroll_to` pattern): only drivers whose bridge can genuinely reach the
        OS clipboard (windows, via raw Win32 `OpenClipboard`/`GetClipboardData`
        — no OLE wrapper, no package identity needed; linux, web and tui through
        their own agents; macOS and iOS through the app's in-process
        `GET /clipboard/text`, one reader on `InProcessAgentDriver`) implement
        it. Attacks the
        "clipboard contents aren't readable headlessly" assumption
        (test_settings_logs.py's log-copy-button check settles for asserting
        the affordance only) rather than accepting it — a real interactive
        desktop session genuinely can read the clipboard.
        """
        raise NotImplementedError(f"{type(self).__name__} does not implement get_clipboard_text()")

    def barrier(self, timeout: float = 10.0) -> None:
        """Block until all UI-thread work enqueued *before* this call has run.

        Convention 14's causal anchor for negative asserts: instead of sleeping N
        seconds and peeking (which false-passes when the would-be event is merely
        late, and taxes every green run when it isn't), read the observable,
        trigger, positively await the trigger's own completion, `barrier()`, and
        only then assert the observable unchanged.

        Concrete with a NotImplementedError default (the `double_click` /
        `get_clipboard_text` pattern) because a barrier is a **cross-app contract
        entry** (convention 11): an app either honours it or refuses loudly. What
        it must never do is ack early — a barrier that returns before the work has
        run silently converts every negative assert built on it back into the race
        it was meant to remove, and nothing downstream can tell. That is why this
        default raises rather than returning quietly.

        The per-app mechanisms are deliberately different (tui drains its
        `UiMessage` channel, linux round-trips a glib idle, web yields two
        macrotask turns) — see `fauna_e2e_agent::BARRIER` for why they are not
        interchangeable.
        """
        raise NotImplementedError(
            f"{type(self).__name__} does not implement barrier() — convention 14's "
            "causal anchor. Implement it on the app's test agent (see "
            "`fauna_e2e_agent::BARRIER`) rather than substituting a sleep."
        )

    def focus_move(self, direction: str = "next", times: int = 1) -> None:
        """Advance the keyboard focus `times` positions (`"next"` / `"prev"`).

        Convention 17 layer (c)'s walk vocabulary, and a convention-11 cross-app
        contract entry on the same terms as `barrier()`: an app either honours it
        or refuses loudly on its own `error-message`.

        **The contract is that this goes through the app's REAL key door** — the
        same handler a human's Tab / Shift-Tab reaches (tui `App::focus_next`,
        linux GTK `child_focus`), never a private setter that assigns a focus
        index. Convention 17 ratified that qualifier because one of its three
        motivating bugs lived exactly in the human-path/agent-path difference, so
        a walk driving a private seam structurally could not have found it.

        Payload vocabulary and its bounds are owned by
        `fauna_e2e_agent::{FOCUS_MOVE, focus_move_request}` — including the
        `FOCUS_MOVE_MAX_TIMES` cap, which exists because the step loop runs on
        the thread that serves the agent, so an unbounded count does not produce
        a slow command but an app on which *every* later command times out.

        **web overrides this default** (`WebBridgeDriver.focus_move`) rather than
        routing through `call_command`: in-page JS has no real key door — an
        untrusted `KeyboardEvent` dispatched from script moves no focus at all
        (measured 2026-08-21) — so the real door is reachable only from the
        Playwright side, pressing real `Tab`/`Shift-Tab` through the bridge's
        `/keyboard/press` route (a trusted CDP key event). Same payload rulings,
        re-derived on the Python side rather than importing the Rust crate.
        """
        self.call_command("focus_move", {"direction": direction, "times": times})

    def switch_pane(self, pane: str) -> None:
        """Hand the keyboard to the nav region (`"sidebar"`) or content (`"page"`).

        The other half of the layer-(c) walk vocabulary, and the reason a walk
        needs more than tabbing: a focus ring alone cannot always cross the
        region boundary. On tui a page pane's ring does not hold the keyboard
        until the zone changes, and a `nav` patch never sets the zone
        (`App::apply` is zone-agnostic by design), so a page reached through the
        state protocol stays in whatever zone the session was already in. A walk
        that cannot cross that boundary explores one region and reports having
        explored the app.

        Owned by `fauna_e2e_agent::{SWITCH_PANE, switch_pane_target}`.

        **web overrides this default** (`WebBridgeDriver.switch_pane`): the DOM
        has no keystroke or API that jumps between landmarks, so it crosses the
        way a keyboard user would — pressing `Tab`/`Shift-Tab` through the real
        door (the same `/keyboard/press` bridge route `focus_move` uses) until
        `document.activeElement` lands inside the target landmark (`<main>` for
        `page`, the `<nav>` not inside `<main>` for `sidebar`), never an in-page
        command.
        """
        self.call_command("switch_pane", {"pane": pane})

    def set_provider_base_urls(self, urls: dict[str, str]) -> None:
        """Override the OnboardingMachine's provider HTTP base URLs.

        Used by tests that need to redirect VPS / DNS / nest HTTP calls
        to a local fake (`tests/e2e-unified/fakes/fake_cloud.py`). Keys:
        "vps", "dns", "nest". Design tracked internally.
        """
        raise NotImplementedError(
            f"{type(self).__name__} does not implement set_provider_base_urls"
        )

    @abstractmethod
    def screenshot(self, name: str) -> Path:
        """Take a screenshot. Returns path to the saved image."""
        ...

    def navigate_to(self, view: str) -> None:
        """Navigate to a view by name. Default: clicks '{view}-tab'."""
        self.click(f"{view}-tab")

    _VIEW_TO_TAB = {
        # NB: this map is currently unused (navigate_to() just clicks
        # "{view}-tab"). There is no `groups` page anymore — group threads live
        # on the conversations page (flavor mlsGroup) — so no groups-tab entry.
        "conversations": "conversations-tab",
        "events": "events-tab",
        "contacts": "contacts-tab",
        "media": "media-tab",
        "backups": "backups-tab",
        "devices": "sync-tab",
        "bridges": "bridges-tab",
        "settings": "settings-tab",
        "feed": "feed-tab",
        "notifications": "notifications-tab",
        "status": "settings-tab",  # status consolidated into settings
    }
