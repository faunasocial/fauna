from __future__ import annotations

import time
from typing import TYPE_CHECKING

from i18n.strings import S

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class BridgesActions:
    """Drive the unified feed-side **Bridges** page (docs/goal/behavior/bridges.md).

    Metadata-driven: list + drill-down detail page served identically to all 7
    apps, rendering each provider's `BridgeStatus` (link modes, settings,
    follows) from server-declared metadata (bridges.md § Layout & flow). This
    is NOT the dedicated Nostr page (nostr.md) — Nostr is excluded from this
    page's bridge list by design (bridges.md § Scope, ratified 2026-06-13; the
    client-side `id != "nostr"` filter lifted to web/linux/android alongside
    this class, 2026-07-17 — apple had it earlier; windows still
    lacks a dedicated Nostr page so keeps showing it generically for now).

    Navigation is the standard `{page}-tab` pattern (`bridges-tab`) — Bridges is
    a standalone sidebar item on all 7 apps (bridges.md § Layout & flow),
    unlike Nostr's nested settings sub-page.

    Per-app shape (bridges.md § Element IDs; `bridge-card` is a real, indexed
    CONTAINER on every app as of — ui.yaml `type:
    view`, `indexed: true` — not merely a grouping of its member elements,
    which is why `link()`/`unlink()` scope their click by the bridge's own
    `bridge-card[index]` — the index of the bridge's own card as the page
    renders it (`_card_index`), never a wire-order position or a bare global
    `bridge-action-button` index):
      * web / android / macos / ios / tui: each bridge renders inline as its
        own card; `bridge-action-button` (+ any `bridge-link-field-{key}`
        inputs) is directly on screen once the page loads — no drill-down click.
        tui registers each bridge's member elements `.within("bridge-card", i)`
        (apps/fauna-tui/src/bridges.rs); web (`BridgeCard.svelte`), android
        (`BridgesScreen.kt`) and apple (`BridgeCardContent`) set a plain
        repeated container id instead, which the driver's own `scope="…[i]"`
        parsing indexes over identically.
      * linux: a NavigationSplitView — the list row (keyed by the bridge's own
        `id` as its AT-SPI/agent name, `views/bridges/list.rs`) must be
        activated first to open the detail pane; its `bridge-action-button`
        then OPENS a modal `bridge-link-form` dialog carrying its own scoped
        `bridge-action-button` to submit (`views/bridges/detail.rs`) — see
        `link()`. Unlink has no such dialog (a direct action). The detail
        pane itself now carries `bridge-card` too (`build_bridge_detail_content`'s
        root box), but `_open_bridge` already replaces it one bridge at a
        time — only `bridge-card[0]` ever resolves on linux, never a real
        index; scoping the click here would be inert, so `link()`/`unlink()`
        leave linux's own click unscoped.
      * windows: `bridge-action-button` sits directly in the list row
        (`Controls/BridgeCard.xaml`, a real `AutomationId="bridge-card"` on
        the row); clicking it always opens a `ContentDialog` (even for a
        zero-field link mode) built with custom Primary/Cancel buttons
        (`Controls/BridgeCardActions.cs` — a `ContentDialog`'s
        template-generated buttons carry no AutomationId and aren't
        automatable). The link dialog mirrors linux exactly: its own scoped
        `bridge-action-button` inside `bridge-link-form` submits (see
        `link()`). Unlink is the one shape no other app has — windows
        confirms it through its own modal (`bridge-unlink-confirm-modal` +
        `bridge-unlink-confirm-button`, ui.yaml `platform_elements.windows`,
        user-approved 2026-07-31 — see `unlink()`).

    `bridge-action-button` is the SAME id for both the link and the unlink
    action (bridges.md § User actions table), so there is no reliable
    client-side "is this bridge linked" signal to poll here — callers verify
    a link/unlink outcome against the server (e.g. `fauna.bridges.list` via
    `ApiActor`), not through this class.

    Every currently-live feed-side provider on this page has exactly ONE link
    mode (Bluesky `oauth`, ActivityPub `enable`) — a multi-mode choice UI
    (like Nostr's 4 modes on its own dedicated page) is unexercised here and
    NOT implemented; bridges.md § Link modes flags per-app multi-mode
    presentation as an open unknown. `link()` below assumes a single
    applicable mode.
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    # --- Navigation ---

    def navigate(self) -> None:
        self.driver.navigate_to("bridges")
        time.sleep(0.5)

    def is_page_visible(self, timeout: float = 10.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.is_visible("page-heading"):
                return True
            time.sleep(0.3)
        return self.driver.is_visible("page-heading")

    # --- Linking ---

    def _open_bridge(self, bridge_id: str) -> None:
        """Bring `bridge_id`'s action button on screen.

        No-op on clients that render every bridge inline (web/android/apple):
        `bridge-action-button` is already visible once the page has loaded.
        On linux, activates the list row keyed by `bridge_id` itself (the raw
        provider id, not a ui.yaml element — see class docstring) to open the
        detail pane."""
        if self.driver.is_linux():
            self.driver.click(bridge_id, wait_for_child="bridge-action-button")

    def _wait_for_linux_action_label(self, bridge_id: str, expect: str,
                                      timeout: float = 10.0) -> None:
        """linux only: poll (re-opening the bridge each attempt) until
        `bridge-action-button`'s label matches `expect` ("Link Bridge" /
        "Unlink Bridge" — `S.bridges.{link,unlink}_bridge`).

        The app refreshes its retained bridge snapshot asynchronously after a
        just-completed link/unlink (`views/bridges/mod.rs`'s row-activated
        handler rebuilds the detail pane, including which of the Link/Unlink
        buttons is visible, from that snapshot — see `detail.rs`). Acting
        again immediately after a mutation can rebuild from a stale snapshot
        and hit the wrong action; re-opening the bridge on each poll gives the
        async refresh room to land before the next click fires. web/android/
        apple update their local state synchronously within the same
        link/unlink handler that performs the mutation, so they never need
        this — only linux calls it.

        Raises ``AssertionError`` — self-diagnosing (e2e rule 6) — if the label
        never settles on ``expect`` within ``timeout``, rather than returning as
        if the snapshot refresh had landed (`e2e-conventions.md` convention 6
        rider).
        """
        deadline = time.monotonic() + timeout
        last_text: str | None = None
        while time.monotonic() < deadline:
            if self.driver.is_visible("bridge-action-button"):
                last_text = self.driver.get_text("bridge-action-button")
                if last_text == expect:
                    return
            self._open_bridge(bridge_id)
            time.sleep(0.3)
        raise AssertionError(
            f"bridge-action-button label did not settle on {expect!r} for "
            f"{bridge_id!r} within {timeout}s (last read: {last_text!r}); "
            f"diagnose={self.driver.diagnose('bridge-action-button')}. The "
            f"link/unlink snapshot refresh cannot be assumed to have landed."
        )

    def _card_index(self, bridge_id: str) -> int:
        """The `bridge-card` index of `bridge_id`'s card, read off what the page
        RENDERS — never off `fauna.bridges.list`'s wire order, which is an
        ordering coupling: it assumes every app paints bridges in wire order and
        clicks the wrong bridge, silently, the first time one sorts by name or
        floats the linked one.

        `bridge-card` carries no per-bridge identity in ui.yaml, so the one card
        a test can name by its bridge is a SOLE card. Today that is every card:
        the unified page renders only `is_unified_bridges_page_bridge` members
        (bridges.md § Element IDs → *Unified-page membership filter*), and of
        the three providers any nest compiles that admits ActivityPub alone —
        whether the nest is a narrow standalone build or the shipped image. A
        second card is a new fact this refuses to guess past: it raises, and
        the fix is a per-card identity element (a ui.yaml addition, rule A).

        Not called on linux, which keys its list rows by `bridge_id` itself and
        shows one detail pane at a time (class docstring)."""
        self.driver.wait_for("bridge-action-button")
        cards = self.driver.count("bridge-action-button")
        if cards != 1:
            raise AssertionError(
                f"cannot address {bridge_id!r}'s bridge-card: the page renders "
                f"{cards} bridge-action-button(s), and bridge-card carries no "
                f"per-bridge identity to tell them apart. A position in "
                f"fauna.bridges.list is not one — give the card an identity "
                f"element (ui.yaml rule A) rather than guessing an index. "
                f"diagnose={self.driver.diagnose('bridge-action-button')}"
            )
        return 0

    def link(self, bridge_id: str, fields: dict[str, str] | None = None) -> None:
        """Link `bridge_id` through the real page UI: open it, fill any
        declared fields, and submit the (single-mode) link action.

        `fields` maps `BridgeLinkField.key` -> value for the bridge's one
        applicable link mode; omit for a zero-field mode (e.g. ActivityPub's
        `enable`).

        On linux AND windows, `bridge-action-button` OPENS a modal
        `bridge-link-form` dialog rather than submitting directly
        (`views/bridges/detail.rs`; `Controls/BridgeCardActions.cs`); that
        dialog carries its own `bridge-action-button` (same id — it's the
        same user action, just realized as this dialog's submit step) scoped
        by `bridge-link-form` to disambiguate it from the card's trigger
        button, which stays in the tree (behind the modal) while the dialog
        is open."""
        self._open_bridge(bridge_id)
        if self.driver.is_linux() or self.driver.is_windows():
            if self.driver.is_linux():
                self._wait_for_linux_action_label(bridge_id, S.bridges.link_bridge)
                self.driver.click("bridge-action-button")
            else:
                index = self._card_index(bridge_id)
                self.driver.click("bridge-action-button", scope=f"bridge-card[{index}]")
            self.driver.wait_for("bridge-link-form")
            for key, value in (fields or {}).items():
                self.driver.clear_and_type(
                    f"bridge-link-field-{key}", value, scope="bridge-link-form")
            self.driver.click("bridge-action-button", scope="bridge-link-form")
        else:
            index = self._card_index(bridge_id)
            for key, value in (fields or {}).items():
                self.driver.clear_and_type(f"bridge-link-field-{key}", value)
            self.driver.click("bridge-action-button", scope=f"bridge-card[{index}]")
        time.sleep(1.0)

    def unlink(self, bridge_id: str) -> None:
        """Unlink `bridge_id` (the same `bridge-action-button`, now in its
        unlink role — see class docstring).

        windows is the only client that confirms unlink through its own modal
        (`bridge-unlink-confirm-modal` + `bridge-unlink-confirm-button`, ui.yaml
        `platform_elements.windows`) — the other 6 unlink directly."""
        self._open_bridge(bridge_id)
        if self.driver.is_linux():
            self._wait_for_linux_action_label(bridge_id, S.bridges.unlink_bridge)
            self.driver.click("bridge-action-button")
        else:
            index = self._card_index(bridge_id)
            self.driver.click("bridge-action-button", scope=f"bridge-card[{index}]")
        if self.driver.is_windows():
            self.driver.wait_for("bridge-unlink-confirm-modal")
            self.driver.click(
                "bridge-unlink-confirm-button", scope="bridge-unlink-confirm-modal")
        time.sleep(1.0)

    # --- The ward's feed-source ask (family-safety.md § Feed-source approvals) ---
    #
    # Rendered only for a supervised account whose `feed_sources` is `block`: a
    # refused link/follow offers `bridge-source-request-button` under its own
    # `bridge-card[i]`, and an ask then reads as `bridge-source-request-state`
    # (pending, or approved = the "try again" prompt — the app never retries).

    def require_source_request_supported(self) -> None:
        """Skip unless this app renders the ward's feed-source ask (e2e
        convention 7 — the platform check lives in the action layer).

        tui led 2026-08-28; macOS and iOS, linux, web and android lifted it
        2026-09-26. windows carries the ids only as generated constants."""
        if self.driver.is_windows():
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the ward's feed-source ask (bridge-source-request-"
                        "button / -state)",
                detail="built on tui, macOS, iOS, linux, web and android; "
                       "windows has the ids as constants only",
                tracked="family-safety.md § Feed-source approvals → App "
                        "affordance",
            )

    def _card_scope(self, bridge_id: str, index: int) -> str:
        """The `bridge-card[i]` scope for `bridge_id`'s card. linux shows one
        bridge's detail pane at a time (class docstring), so its card is always
        `[0]` once opened."""
        self._open_bridge(bridge_id)
        return "bridge-card[0]" if self.driver.is_linux() else f"bridge-card[{index}]"

    def source_request_offered(self, bridge_id: str, index: int = 0) -> bool:
        return self.driver.count(
            "bridge-source-request-button", scope=self._card_scope(bridge_id, index)
        ) > 0

    def request_source(self, bridge_id: str, index: int = 0) -> None:
        self.driver.click(
            "bridge-source-request-button", scope=self._card_scope(bridge_id, index)
        )

    def source_request_states(self, bridge_id: str, index: int = 0) -> list[str]:
        """Every ask-state label on the card, in order ("" entries dropped)."""
        return [
            t for t in self.driver.get_texts(
                "bridge-source-request-state", scope=self._card_scope(bridge_id, index)
            )
            if t
        ]

    # --- Errors ---

    def page_error_text(self, timeout: float = 8.0) -> str:
        """Return the page's error message once it appears, else "".

        Mirrors the shared `App.error_text()` / `NostrActions.page_error_text`
        contract: state-protocol `messages.error` first, `error-message`
        element as fallback.

        `timeout=0` means "whatever the value is right now" — one read, no
        waiting. A caller capturing a *transient* error (one a later refresh
        will clear, e.g. the Bridges page's `ErrorMessage = null` reset) needs
        that snapshot, so the loop must run at least once regardless of the
        deadline."""
        deadline = time.monotonic() + timeout
        while True:
            try:
                from_state = self.driver.get_state("messages.error")
            except Exception:
                from_state = None
            if from_state:
                return str(from_state)
            if self.driver.is_visible("error-message"):
                txt = self.driver.get_text("error-message")
                if txt and txt.strip():
                    return txt
            if time.monotonic() >= deadline:
                return ""
            time.sleep(0.3)
