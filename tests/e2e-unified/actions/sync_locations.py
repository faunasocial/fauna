from __future__ import annotations

import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class SyncLocationsActions:
    """Seed the location ↔ folder binding rows nested under each folder on
    Settings → Folders (file-sync.md § On-Demand Files; ui.yaml `folders`
    `platform_elements` — the shared ``folder-location-*`` contract, desktop-only).

    The former free-standing flat ``Settings → Sync`` page this class used to drive
    is retired on every desktop app (linux/windows/macOS all migrated to the
    nested-per-set binding at the 2026-06-28 unification); the nested tests live in
    ``test_folders.py`` (``test_bind_location_nested_under_folder[_windows|_macos]``)
    and drive the ``folder-location-*`` ids directly, scoped to the expanded
    ``folder-row``. The one thing still uniform enough to warrant a shared helper
    is seeding: the bound-location list is seeded via the ``sync_inject_locations``
    test-state-injection command (the ``conversations_inject_inbound`` pattern), NOT
    a live sync helper — the real add flow opens a native OS directory picker the e2e
    drivers cannot drive (Windows ``FolderPicker`` / linux ``FileChooserDialog`` /
    macOS ``NSOpenPanel``), and the C#↔Rust pipe wire codec is already pinned by the
    deterministic ``DagCborIpcTests``. Each desktop app implements the same
    command (priority #1), so this seam is uniform across the apps that use it.
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def inject_locations(self, locations: list[dict]) -> None:
        """Seed the bound-location list. Each entry is a dict with keys ``path``,
        ``folder`` and ``mode`` (``"always"`` | ``"on-demand"``).

        ⚠ An entry with no ``folder`` is **skipped on windows** since the shared
        binding model (``fauna_client_sync::agent::LocationBindingsModel``) holds only
        *bindings* — an added-but-unbound location is not a renderable row, and ui.yaml
        retired the free-text set name when binding became contextual under a set.
        Pass a ``folder`` for any row you expect to render. Routed through
        the per-app ``sync_inject_locations`` TestAgent command, which swaps the
        binding source onto an in-memory fake holding these rows."""
        self.driver.call_command("sync_inject_locations", {"locations": locations})

    def prepare_binding_channel(self) -> None:
        """Put the Folders page's binding source where a binding test can drive it.

        windows runs no live `fauna-sync-agent` under e2e, so its page is swapped
        onto the in-memory location channel (:meth:`inject_locations` with an
        empty list, BEFORE the first navigate — the shape
        `test_bind_location_nested_under_folder_windows` established). tui (and
        linux) direct-spawn the REAL agent into the launch's isolated world
        (`drivers/tui.py`), so there is nothing to swap: the binding goes over
        the real control plane. One call site, the per-app difference owned here.
        """
        if self.driver.is_windows():
            self.inject_locations([])

    # --- Per-binding on-demand mode (`folder-location-mode-toggle`) -----------
    #
    # ui.yaml § folders: windows and linux, plus tui on a windows host. The
    # on-demand affordance there is LOCATION-level — the switch sits on the bound
    # row and flips that binding between always-resident and the agent's
    # placeholder root (cfapi on windows, a FUSE mount on linux;
    # on-demand-files.md § On-Demand Files (Placeholders)). A fresh binding on a
    # windows host starts on-demand (user ruling 2026-09-26) and on linux
    # always-resident, whichever app binds, since the default is the agent's.
    # Apple's set-level `folder-on-demand-toggle` is the sanctioned deviation
    # (actions/backups.py); tui renders the switch only where its own predicate
    # says its agent has a placeholder surface (windows today).

    def mode_toggle_visible(self, *, scope: str | None = None) -> bool:
        """Whether the bound row renders ``folder-location-mode-toggle``.

        ``is_visible_scrolled``, not a bare ``is_visible``: the toggle sits in a
        location row nested inside an expanded folder row, below every other
        expanded-body control, so on windows' 600 DIP e2e window it is routinely
        past the fold (the same trap `webdav_toggle_visible` documents).
        """
        return self.driver.is_visible_scrolled("folder-location-mode-toggle", scope=scope)

    def mode_toggle_state(self, *, scope: str | None = None) -> str | None:
        """``"always"`` / ``"on-demand"`` — the binding's mode as the row renders it.

        Read over the uniform ``/element/attr?attr=state`` contract, which the
        windows bridge answers from `AutomationProperties.HelpText` — the page
        stamps the row's mode there when it builds the toggle, and rebuilds the
        row from the binding model after every mutation, so the read is the
        model's mode, never surviving view state. tui answers it from the
        element's `state` attr, painted each frame from the shared binding
        model's agent-mirrored `mode`; linux from the `state` marker the row
        stamps on its switch when it is rebuilt from that same model. Returns
        ``None`` when the
        element is absent; assert :meth:`mode_toggle_visible` first when "always"
        must be told from "not rendered".
        """
        return self.driver.get_attr("folder-location-mode-toggle", "state", scope=scope)

    def toggle_mode(self, *, scope: str | None = None, timeout: float = 30.0) -> str | None:
        """Flip the bound row's on-demand switch, returning the settled mode.

        The click drives the real page path — on windows
        `LocationsViewModel.SetModeAsync` → the controller → the location control
        channel's `SetLocationSyncMode`; on tui the agent surface's
        `set_location_mode` → the REAL agent's `SetLocationSyncMode`, which
        re-drives its engines (the cfapi root comes down or up) — then the
        reconcile re-lists the bindings and the page rebuilds the row with the
        agent's mode. The default ceiling is generous for that real round trip on
        a loaded box — a ceiling, not an expectation.

        Convention 14: the wait is a deadline poll on the toggle's own state,
        not a settle-sleep. The rebuilt row is a NEW toggle whose HelpText
        carries the new mode; until the rebuild lands the read still answers
        from the old one, so polling for "no longer reads what it read before"
        is latency-independent and cannot pass by merely waiting long enough.
        """
        before = self.mode_toggle_state(scope=scope)
        self.driver.is_visible_scrolled("folder-location-mode-toggle", scope=scope)
        self.driver.click("folder-location-mode-toggle", scope=scope)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            settled = self.mode_toggle_state(scope=scope)
            if settled is not None and settled != before:
                return settled
            # sleep-ok: poll interval inside the deadline loop above, not a
            # settle-sleep — each tick re-reads the rebuilt row and returns the
            # instant the flip is visible (convention 14's deadline-poll shape).
            time.sleep(0.2)
        return self.mode_toggle_state(scope=scope)
