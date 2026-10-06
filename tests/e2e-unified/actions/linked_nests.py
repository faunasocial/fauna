from __future__ import annotations

import re
import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver

# A WinUI InfoBar's accessible name is "<Severity> icon <message>" — the
# severity glyph's own name, not part of the message. The `messages.error`
# state path never carries it, so a caller that lands on the element
# fallback must see the same chrome-free text the state path would have
# given, or a downstream exact/substring match (e.g. against a raw log
# line) breaks on windows only. Same latent gap in the five sibling
# `page_error_text()` copies (bridges.py, mail_aliases.py, nostr.py,
# mail_settings.py, atproto_settings.py) — none currently has a red test
# proving it, so left alone here.
_SEVERITY_ICON_PREFIX = re.compile(r"^\S+ icon\s+")


class LinkedNestsActions:
    """Drive the user-facing "Nests" page — linking half (docs/goal/behavior/linked-nests.md).

    A user-settings surface (NOT the admin shell) where a user links one of
    their own nests to sync their account's content (per-user multi-homing),
    lists their linked nests, and unlinks them. Renders the shared
    `LinkedNestsMachine` (libs/fauna-client-pair, over UniFFI/wasm): Refresh →
    `fauna.pair.list`, Link → `fauna.pair.add`, Unlink → `fauna.pair.revoke`.
    Same IDs + flows on all seven apps (priority #1); linux leads.

    The trust facet on the same page (Now/History lens, grants, revoke) is driven
    by `NestTrustActions` (actions/nest_trust.py).

    The page + IDs were renamed `linked-nests-*` → `nests-*` (ui.yaml 2026-07-07,
    all seven apps by 2026-07-15), and the nav id (`set_state` sub-page slug)
    followed fleet-wide 2026-10-02: it is `nests`, the one id this action sends
    to every app.
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        """Navigate to the Nests surface.

        On linux the page is embedded in the settings/status view (like
        mail-settings — the adw::PreferencesWindow modal isn't reachable through
        the state protocol), so navigating to "settings" surfaces the page IDs.
        The list hydrates asynchronously (`fauna.pair.list`), so callers poll the
        add-button / rows rather than a fixed wait.

        Page-based clients (android renders a sub-page; ios/macos/windows to
        follow) reach the surface from a settings entry (`*-link`) rather than
        embedding it inline. When the add-button isn't already on screen, click
        into the sub-page; this is a safe no-op for the embedded clients
        (linux/web), whose add-button is already present so the branch is skipped.

        The nav sub-page id is `nests` (the one slug the shared action sends to
        every app, per the class docstring).
        """
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "nests"}]},
        })
        time.sleep(1)
        if not self.driver.is_visible("nests-add-button"):
            for link in ("nests-link", "linked-nests-link"):
                try:
                    if self.driver.is_visible(link):
                        self.driver.click(link)
                        time.sleep(1)
                        break
                except Exception:
                    pass

    def is_page_visible(self, timeout: float = 10.0) -> bool:
        """True once the page is reachable (the add-a-nest button is present).

        Uses wait_for so the driver scrolls the button into the viewport — the
        section sits below other settings sections in the embedded view, so GTK
        may not have rendered it into the AT-SPI tree until it's scrolled
        on-screen.
        """
        try:
            self.driver.wait_for("nests-add-button", timeout=timeout)
            return True
        except TimeoutError:
            return False

    def pairing_count(self) -> int:
        """How many *linked* nests the list renders — counted by unlink buttons.

        On a trust-enabled client (web, and the others as they migrate) the home
        nest is also a `*-item` row, but it carries no unlink button (it is the
        user's own nest, not a pairing). Every pairing carries exactly one
        `*-item-unlink-button`, so counting those yields the pairing count on both
        trust and non-trust clients — unlike counting `*-item`, which would
        conflate the home row.
        """
        return self.driver.count("nests-item-unlink-button")

    def nest_ids(self) -> list[str]:
        """The abbreviated nest-id text for each *pairing* row (`*-item-nest-id`).

        The home row (trust clients) is rendered first and has no unlink button;
        the pairing rows are therefore the last `pairing_count()` id entries. On a
        non-trust client (no home row) that slice is simply all rows.
        """
        tid = "nests-item-nest-id"
        all_ids = [t or "" for t in self.driver.get_texts(tid)]
        pairings = self.pairing_count()
        return all_ids[-pairings:] if pairings else []

    def link(self, nest_id: str) -> None:
        """Link a nest: reveal the add form (`*-add-button`), type the nest
        identity (`*-add-input`), submit (`*-add-submit-button`).

        Submit dispatches `LinkedNestsAction::Link` (→ `fauna.pair.add`, default
        full self-sync capabilities) through the shared machine; the add reloads
        the list, so the new row appears once the async round-trip settles.
        Callers poll `wait_for_pairing_count` / `page_error_text` for the outcome.
        """
        self.driver.wait_for("nests-add-button", timeout=10.0)
        self.driver.click("nests-add-button")
        self.driver.wait_for("nests-add-input", timeout=10.0)
        self.driver.clear_and_type("nests-add-input", nest_id)
        self.driver.click("nests-add-submit-button")
        time.sleep(1)

    def unlink(self, index: int = 0) -> None:
        """Unlink the nest at `index` via its `*-item-unlink-button`
        (→ `LinkedNestsAction::Unlink` → `fauna.pair.revoke`); the list reloads."""
        self.driver.click("nests-item-unlink-button", index=index)
        time.sleep(1)

    # ── Forward queue (ui/nests.md § Forward queue) ─────────────────────────
    # Page-level, conditional: the block exists only while the connected nest
    # reports posts of the user's still waiting to reach its relay.

    def forward_queue_text(self) -> str | None:
        """The `nests-forward-queue` status line, or ``None`` while the block
        is absent (nothing queued, or a nest too old to report a queue)."""
        if not self.driver.is_visible("nests-forward-queue"):
            return None
        return self.driver.get_text("nests-forward-queue") or ""

    def forward_queue_reason(self) -> str | None:
        """The `nests-forward-queue-reason` line — the nest's own record of the
        latest failed attempt — or ``None`` until a send has failed."""
        if not self.driver.is_visible("nests-forward-queue-reason"):
            return None
        return self.driver.get_text("nests-forward-queue-reason") or ""

    def retry_forwards(self) -> None:
        """Press `nests-forward-retry-button` (→ `fauna.pair.forward_retry`,
        then the page re-lists)."""
        self.driver.click("nests-forward-retry-button")

    def discard_forwards(self) -> None:
        """Press `nests-forward-discard-button` (→ `fauna.pair.forward_discard`,
        then the page re-lists)."""
        self.driver.click("nests-forward-discard-button")

    def wait_for_pairing_count(self, expected: int, timeout: float = 12.0) -> bool:
        """Poll until the list holds exactly `expected` rows (the dispatch →
        `fauna.pair.list` → re-render round-trip is async)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.pairing_count() == expected:
                return True
            time.sleep(0.3)
        return self.pairing_count() == expected

    def page_error_text(self, timeout: float = 8.0) -> str:
        """Return the page's error message once it appears, else "".

        Reads the state protocol's `messages.error` first (the canonical
        cross-app error surface, serialized live from the active page's
        error state), falling back to the `error-message` element. Surfaces the
        operator-policy rejection (`fauna.pair.pairing_disabled`) the machine
        projects into `snapshot.error` when pairing is disabled nest-wide.

        State-first matters on windows: `error-message` is a WinUI InfoBar
        whose UIA peer is unreliable after an IsOpen toggle (FlaUI may not read
        it), while `messages.error` always reflects `App.CurrentErrorMessage`.
        Mirrors the shared `App.error_text()` contract.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                from_state = self.driver.get_state("messages.error")
            except Exception:
                from_state = None
            if from_state:
                return str(from_state)
            if self.driver.is_visible("error-message"):
                txt = self.driver.get_text("error-message")
                if txt and txt.strip():
                    return _SEVERITY_ICON_PREFIX.sub("", txt)
            time.sleep(0.3)
        return ""
