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
        """Every app renders the ward's feed-source ask, so nothing skips (e2e
        convention 7 — the platform check lives in the action layer, and this is
        where a future gap would be declared).

        tui led 2026-08-28; macOS and iOS, linux, web and android lifted it
        2026-09-26, and windows last."""

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

    # --- The linked card: identity, settings, follows (bridges.md § Link modes,
    # § Bridge settings, § Follows) ---
    #
    # A setting row is `bridge-setting[<key>]`, keyed by the setting itself, so
    # a test names the setting it means and never its position in the list. A
    # change saves on the spot: a checkbox on its click, a number or text field
    # on type-then-click (the agent's click on an editable is its commit).

    def identity_text(self, bridge_id: str) -> str:
        scope = self._card_scope(bridge_id, self._card_index(bridge_id))
        self.driver.wait_for("bridge-identity", scope=scope)
        return self.driver.get_text("bridge-identity", scope=scope)

    def has_setting(self, bridge_id: str, key: str) -> bool:
        scope = self._card_scope(bridge_id, self._card_index(bridge_id))
        return self.driver.count(f"bridge-setting[{key}]", scope=scope) > 0

    def setting_checked(self, bridge_id: str, key: str) -> str | None:
        """A boolean setting's `checked` attr ("true"/"false")."""
        scope = self._card_scope(bridge_id, self._card_index(bridge_id))
        return self.driver.get_attr(f"bridge-setting[{key}]", "checked", scope=scope)

    def setting_text(self, bridge_id: str, key: str) -> str:
        scope = self._card_scope(bridge_id, self._card_index(bridge_id))
        return self.driver.get_text(f"bridge-setting[{key}]", scope=scope)

    def toggle_setting(self, bridge_id: str, key: str) -> None:
        scope = self._card_scope(bridge_id, self._card_index(bridge_id))
        self.driver.click(f"bridge-setting[{key}]", scope=scope)

    def commit_setting(self, bridge_id: str, key: str, value: str) -> None:
        scope = self._card_scope(bridge_id, self._card_index(bridge_id))
        self.driver.clear_and_type(f"bridge-setting[{key}]", value, scope=scope)
        self.driver.click(f"bridge-setting[{key}]", scope=scope)

    def add_follow(self, bridge_id: str, external_id: str, petname: str = "") -> None:
        scope = self._card_scope(bridge_id, self._card_index(bridge_id))
        self.driver.clear_and_type("bridge-follow-id-input", external_id, scope=scope)
        if petname:
            self.driver.clear_and_type(
                "bridge-follow-petname-input", petname, scope=scope)
        self.driver.click("bridge-add-follow-button", scope=scope)

    def follow_rows(self, bridge_id: str) -> list[str]:
        """Every follow's row text, in order (the nickname, else the id)."""
        return self.driver.get_texts(
            "bridge-follow-item",
            scope=self._card_scope(bridge_id, self._card_index(bridge_id)))

    def remove_follow(self, bridge_id: str, index: int = 0) -> None:
        scope = self._card_scope(bridge_id, self._card_index(bridge_id))
        # Opening a card can rebuild it (linux) and read its follows again, so
        # the row is not there until that reply lands: wait for it, never click
        # into the gap (convention 14).
        self.driver.wait_for("bridge-follow-remove", scope=scope)
        self.driver.click("bridge-follow-remove", index, scope=scope)

    # --- Fediverse follow requests (bridges.md § Follow requests) ---
    #
    # The list sits on the linked card under the follows list, for a provider
    # declaring `supports_follow_requests` (ActivityPub). Its heading carries
    # the count; each row is the requester's address and name with Approve and
    # Refuse (no confirm). Native apps render the card inline, so the card is
    # `bridge-card[0]` — the sole card (`_card_index`).

    def follow_requests_heading(self, bridge_id: str) -> str:
        return self.driver.get_text(
            "bridge-follow-requests-list", scope=self._card_scope(bridge_id, 0))

    def follow_request_rows(self, bridge_id: str) -> list[str]:
        """Every waiting request's row text, in order."""
        return self.driver.get_texts(
            "bridge-follow-request-item", scope=self._card_scope(bridge_id, 0))

    def approve_follow_request(self, bridge_id: str, index: int = 0) -> None:
        self.driver.click(
            "bridge-follow-request-approve", index,
            scope=self._card_scope(bridge_id, 0))

    def refuse_follow_request(self, bridge_id: str, index: int = 0) -> None:
        self.driver.click(
            "bridge-follow-request-refuse", index,
            scope=self._card_scope(bridge_id, 0))

    # --- Hosted plugins (ADMIN ONLY; bridges.md § Hosted plugins on this page) ---

    def install_plugin(self, document_url: str) -> None:
        """Type the plugin's document URL and press Install — the nest opens
        the install card in Connected apps → Requests."""
        self.driver.wait_for("bridge-plugin-install-url", timeout=10.0)
        self.driver.clear_and_type("bridge-plugin-install-url", document_url)
        self.driver.click("bridge-plugin-install-submit")

    def plugin_count(self) -> int:
        """Counted by the name leaf, one per item on every app: tui's
        `bridge-plugin-item` is a scope container only (the `bridge-card`
        shape), which an unscoped count does not see."""
        return self.driver.count("bridge-plugin-name")

    def _plugin_scope(self, index: int) -> str:
        return f"bridge-plugin-item[{index}]"

    def plugin_name(self, index: int) -> str:
        return self.driver.get_text("bridge-plugin-name", scope=self._plugin_scope(index)) or ""

    def plugin_publisher(self, index: int) -> str:
        return (
            self.driver.get_text("bridge-plugin-publisher", scope=self._plugin_scope(index))
            or ""
        )

    def plugin_state(self, index: int) -> str:
        """The runner's state VALUE — the status leaf's `state` attr."""
        return (
            self.driver.get_attr(
                "bridge-plugin-status", "state", scope=self._plugin_scope(index)
            )
            or ""
        )

    def plugin_index_named(self, name: str) -> int | None:
        for i in range(self.plugin_count()):
            if self.plugin_name(i) == name:
                return i
        return None

    def wait_for_plugin(
        self, name: str, state: str | None = None, timeout: float = 30.0
    ) -> int | None:
        """Wait for the list to show `name` (in `state`, when given),
        re-reading by revisiting the page; its index or None. Deadline-polled
        (convention 14)."""
        deadline = time.monotonic() + timeout
        while True:
            i = self.plugin_index_named(name)
            if i is not None and (state is None or self.plugin_state(i) == state):
                return i
            if time.monotonic() >= deadline:
                return None
            self.navigate()

    def wait_for_no_plugin(self, name: str, timeout: float = 30.0) -> bool:
        deadline = time.monotonic() + timeout
        while True:
            if self.plugin_index_named(name) is None:
                return True
            if time.monotonic() >= deadline:
                return False
            self.navigate()

    def uninstall_plugin(self, index: int) -> None:
        """Uninstall: the first press arms the inline confirm, the second
        uninstalls."""
        scope = self._plugin_scope(index)
        self.driver.click("bridge-plugin-uninstall", scope=scope)
        self.driver.click("bridge-plugin-uninstall", scope=scope)

    # --- A plugin's settings card (EVERY account; third-party.md § Hosted
    # principals). Each installed plugin is one more `bridge-card` (owner-ruled
    # 2026-10-16: no card family of its own); its `bridge-action-button`
    # carries the plugin's `client_id` and a `bound` value, which is how a
    # reader tells the cards apart — `bridge-card` has no per-card identity.
    # Built on tui; the other apps ride the trickle-down. ---

    def plugin_card_index(self, client_id: str) -> int | None:
        """The `bridge-card` index of `client_id`'s settings card, read off the
        action button the page renders, or None."""
        for i, cid in enumerate(self.driver.get_attrs("bridge-action-button", "client_id")):
            if cid == client_id:
                return i
        return None

    def plugin_bound(self, client_id: str) -> bool | None:
        """Whether this account has bound the plugin (its card's button reads
        Disconnect), or None when no card names it."""
        i = self.plugin_card_index(client_id)
        if i is None:
            return None
        return self.driver.get_attr("bridge-action-button", "bound", i) == "true"

    def wait_for_plugin_bound(
        self, client_id: str, bound: bool, timeout: float = 30.0
    ) -> bool:
        """Wait for `client_id`'s card to read `bound`, re-reading by revisiting
        the page (a bind approved on Connected apps pushes nothing here).
        Deadline-polled (convention 14)."""
        deadline = time.monotonic() + timeout
        while True:
            if self.plugin_bound(client_id) is bound:
                return True
            if time.monotonic() >= deadline:
                return False
            self.navigate()

    def press_plugin_card(self, client_id: str) -> None:
        """Press the plugin card's action button — Connect while unbound (the
        bind card then waits in Connected apps → Requests), Disconnect while
        bound."""
        i = self.plugin_card_index(client_id)
        if i is None:
            raise AssertionError(
                f"no settings card names {client_id!r}; "
                f"diagnose={self.driver.diagnose('bridge-action-button', attrs=('client_id',))}"
            )
        self.driver.click("bridge-action-button", scope=f"bridge-card[{i}]")

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
