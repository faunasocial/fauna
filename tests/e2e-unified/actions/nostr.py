from __future__ import annotations

import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class NostrActions:
    """Drive the standalone **Nostr** settings page (docs/goal/ui/nostr.md).

    A dedicated user-settings sub-page — the same treatment as mail
    (`mail-settings.md`), NOT folded into the unified Bridges page (page
    structure ratified 2026-06-13). Renders the shared FaunaKit
    `NostrSettingsView` over `NostrVM`; the control plane rides the unified
    `fauna.bridges.*` WS-RPC keyed `bridge_id:"nostr"` (nostr.md § WS-RPC
    migration contract — link/unlink/set_settings/list/list_follows/
    add_follow/remove_follow). web/linux lead; macOS + iOS lifted the page
    2026-06-14 (shared FaunaKit). Same IDs + flows on all seven apps
    (priority #1).

    Flow gates the caller MUST respect (`NostrSettingsView.body`):
      * `!status.available` — `NostrProvider::available` (`nostr_bridging_
        available`) gates on **a user's deposited nsec**
        (`db::any_nsec_deposited`, `bins/fauna-nest/src/nostr/mod.rs:68-74`),
        migrated (S8.9) off the earlier legacy `nest_mode`-row translation. A
        fresh e2e nest is `!available` (no depositor yet), and this class's
        own `link_generate()`/import flow is the normal, supported way to
        satisfy the gate: linking with a generated or imported key calls
        `link_account` with the encrypted privkey, which is exactly what
        `any_nsec_deposited` reads (`bins/fauna-nest/src/nostr/db.rs:566`).
        No known gap remains here.
      * unlinked → only the account-link form (`nostr-link-mode` +
        `nostr-link-button`; `nostr-nsec-input` shows in import mode only).
      * linked → `nostr-pubkey-copy-btn` + `nostr-unlink-button`, the 5
        content toggles, the relays section, the follows section.

    DMs are NOT on this page — a Nostr DM is a bridged room on the unified
    Conversations surface (`test_bridged_conversation.py` covers it).
    """

    # The 5 content-publishing toggles (NostrSettingsView.contentSettingsSection).
    #
    # Deliberately spelled out here even though the apps all render from the
    # shared `fauna_client_bridges::nostr_content_toggle_options` catalog since
    # 2026-08-29 (`docs/goal/ui/nostr.md` § Where logic lives): a test that
    # derived its expectations from the table under test would assert nothing.
    # This list is the independent statement of the contract — if the catalog
    # ever grows or reorders a row, THIS is what must be updated to agree.
    CONTENT_TOGGLES = (
        "nostr-expose-content",
        "nostr-auto-publish",
        "nostr-publish-replies",
        "nostr-publish-reactions",
        "nostr-inbound-to-feed",
    )

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    # --- Navigation ---

    def navigate(self) -> None:
        """Navigate to the Nostr settings sub-page.

        The cross-app two-element nav stack
        `{"view":"settings"},{"view":"settings","id":"nostr"}` selects the
        sub-page on the sidebar-swap shell (macOS `SettingsShellView` →
        `SettingsPage.nostr`) and the iOS `SettingsView` `NavigationStack`
        (`applyNavPatch` reads `stack[1].id` → `selectedSettingsPage`). When the
        page isn't on screen yet (iOS reaches it from a settings row rather than
        a rail), fall back to clicking the `nostr-settings-link` row — a safe
        no-op where the rail already swapped (same pattern as linked-nests).
        """
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "nostr"}]},
        })
        time.sleep(1)
        if not self._on_nostr_page():
            try:
                self.driver.click("nostr-settings-link")
                time.sleep(1)
            except Exception:
                pass

    def _on_nostr_page(self) -> bool:
        """True when the Nostr page's account section is on screen.

        On a plaintext (available) nest the page shows EITHER the link form
        (`nostr-link-button`, unlinked) OR the linked account
        (`nostr-pubkey-copy-btn`) — so one of those being visible is the
        reliable "we're on the Nostr page" signal, independent of link state.
        (`page-heading` is too weak — every settings page renders one.)
        """
        return (
            self.driver.is_visible("nostr-link-button")
            or self.driver.is_visible("nostr-pubkey-copy-btn")
        )

    def is_page_visible(self, timeout: float = 10.0) -> bool:
        """Poll until the Nostr page is reachable (its account section renders)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self._on_nostr_page():
                return True
            time.sleep(0.3)
        return self._on_nostr_page()

    # --- Account linking ---

    def is_linked(self) -> bool:
        """True when the account is linked (the pubkey/copy affordance shows)."""
        return not self.driver.is_absent("nostr-pubkey-copy-btn")

    def link_generate(self) -> None:
        """Link via a freshly generated keypair.

        `NostrVM.LinkMode` defaults to `.generate` (NostrVM.swift), so no mode
        select is needed — just click `nostr-link-button`. The generate →
        `fauna.bridges.link` → `refresh` round-trip is async; poll
        `wait_for_linked` for the outcome.
        """
        self.driver.wait_for("nostr-link-button", timeout=10.0)
        self.driver.click("nostr-link-button")

    def wait_for_linked(self, timeout: float = 20.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.is_linked():
                return True
            time.sleep(0.4)
        return self.is_linked()

    def wait_for_unlinked(self, timeout: float = 20.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.is_visible("nostr-link-button"):
                return True
            time.sleep(0.4)
        return self.driver.is_visible("nostr-link-button")

    def ensure_linked(self) -> bool:
        """Link via generate if not already linked. Returns True once linked.

        A freshly generated account starts with no relays and no follows, so
        callers get a clean slate per link.
        """
        if self.is_linked():
            return True
        self.link_generate()
        return self.wait_for_linked()

    def unlink(self) -> None:
        """Unlink the account (`nostr-unlink-button` — a plain destructive
        Button, no confirm overlay; → `fauna.bridges.unlink` → refresh)."""
        self.driver.wait_for("nostr-unlink-button", timeout=10.0)
        self.driver.click("nostr-unlink-button")

    # --- Content toggles ---

    def toggle_state(self, toggle_id: str) -> str:
        """A content toggle's on/off, read via the `state` attr ("on"/"off").

        The toggles are plain-label SwiftUI `Toggle`s whose
        `accessibilityIdentifier` lands on the underlying Switch; the
        apple-bridge maps the native switch value to the "on"/"off" `state`
        contract (NostrSettingsView.settingToggle — same pattern as the
        mail serve-here / web-subdomain toggles).
        """
        return self.driver.get_attr(toggle_id, "state") or ""

    def set_toggle(self, toggle_id: str, on: bool) -> None:
        """Drive a content toggle to `on`/off idempotently."""
        self.driver.wait_for(toggle_id, timeout=10.0)
        want = "on" if on else "off"
        if self.toggle_state(toggle_id) != want:
            self.driver.click(toggle_id)

    def wait_for_toggle_state(self, toggle_id: str, want: str,
                              timeout: float = 12.0) -> bool:
        """Poll until the toggle reads `want` ("on"/"off").

        The flip round-trips through `fauna.bridges.set_settings` → refresh
        (non-optimistic — the switch reflects the persisted status, not the
        tap), so poll rather than reading once.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.toggle_state(toggle_id) == want:
                return True
            time.sleep(0.3)
        return self.toggle_state(toggle_id) == want

    # --- Relays ---

    def relay_count(self) -> int:
        return self.driver.count("nostr-relay-item")

    def relay_texts(self) -> list[str]:
        return [t or "" for t in self.driver.get_texts("nostr-relay-item")]

    def add_relay(self, url: str) -> None:
        """Type a relay URL (`nostr-relay-input`) and add it (`nostr-add-relay`).

        `NostrVM.addRelay` validates the `wss://`/`ws://` prefix client-side;
        on success it read-modify-writes the `relay_list` JSON array over
        `fauna.bridges.set_settings` then refreshes (async — poll
        `wait_for_relay_count`). A non-`wss`/`ws` URL surfaces an error and
        adds no row.
        """
        self.driver.wait_for("nostr-relay-input", timeout=10.0)
        self.driver.clear_and_type("nostr-relay-input", url)
        self.driver.click("nostr-add-relay")
        time.sleep(0.5)

    def remove_relay(self, index: int = 0) -> None:
        self.driver.click("nostr-remove-relay", index=index)
        time.sleep(0.5)

    def clear_relays(self, timeout: float = 12.0) -> None:
        """Remove every relay row so the list starts from a known-empty state
        (mirrors the linked-nests "start from a known-empty list" discipline —
        the session nest is shared)."""
        deadline = time.monotonic() + timeout
        while self.relay_count() > 0 and time.monotonic() < deadline:
            self.remove_relay(0)
            self.wait_for_relay_count(self.relay_count() - 1, timeout=4.0)
        remaining = self.relay_count()
        if remaining:
            raise AssertionError(
                f"clear_relays: {remaining} relay row(s) still present after "
                f"{timeout}s — the list is not known-empty; "
                f"{self.driver.diagnose('nostr-relay-item')}"
            )

    def wait_for_relay_count(self, expected: int, timeout: float = 12.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.relay_count() == expected:
                return True
            time.sleep(0.3)
        return self.relay_count() == expected

    # --- Follows ---

    def follow_count(self) -> int:
        return self.driver.count("nostr-follow-item")

    def add_follow(self, pubkey: str, petname: str = "") -> None:
        """Add a follow: type the pubkey (`nostr-follow-pubkey-input`) and an
        optional petname (`nostr-follow-petname-input`), submit
        (`nostr-add-follow`).

        The nest's `add_follow` accepts an npub OR raw hex pubkey
        (`bridge_provider.rs`); a 64-hex key stores as-is. The rendered row
        shows the re-encoded npub, so assert on row COUNT, not text.
        """
        self.driver.wait_for("nostr-follow-pubkey-input", timeout=10.0)
        self.driver.clear_and_type("nostr-follow-pubkey-input", pubkey)
        if petname:
            self.driver.clear_and_type("nostr-follow-petname-input", petname)
        self.driver.click("nostr-add-follow")
        time.sleep(0.5)

    def remove_follow(self, index: int = 0) -> None:
        self.driver.click("nostr-remove-follow", index=index)
        time.sleep(0.5)

    def clear_follows(self, timeout: float = 12.0) -> None:
        """Remove every follow row so the list starts from a known-empty state."""
        deadline = time.monotonic() + timeout
        while self.follow_count() > 0 and time.monotonic() < deadline:
            self.remove_follow(0)
            self.wait_for_follow_count(self.follow_count() - 1, timeout=4.0)
        remaining = self.follow_count()
        if remaining:
            raise AssertionError(
                f"clear_follows: {remaining} follow row(s) still present after "
                f"{timeout}s — the list is not known-empty; "
                f"{self.driver.diagnose('nostr-follow-item')}"
            )

    def wait_for_follow_count(self, expected: int, timeout: float = 12.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.follow_count() == expected:
                return True
            time.sleep(0.3)
        return self.follow_count() == expected

    # --- Zap signers (the NIP-57 trust root) ---
    #
    # The *Zap signers* section (`nostr-zap-signer-*`, monetization.md § Zap
    # receipts — the trust model, ratified 2026-07-29). A payee designates which
    # signer pubkey(s) may speak for their money; a payee who has designated
    # nobody believes nobody, which is the ratified out-of-the-box default and
    # why the empty state carries its own id rather than being a blank list.
    # The verbs ride `fauna.nostr.zap_signers.{list,add,remove}`.
    #
    # ⚠ The nest normalizes a pubkey to lowercase on write and the `add` reply
    # carries the STORED row — only that form ever matches a receipt. So a test
    # that types uppercase must assert the rendered row is lowercase, which is
    # exactly the "render the returned row, not the input" contract the shared
    # faces document.

    def is_zap_signer_section_visible(self) -> bool:
        """Is the Zap signers section rendered? Keyed on the add button, which
        is present whenever the section is (the roster may legitimately be
        empty)."""
        if self.driver.count("nostr-zap-signer-add-btn") == 0:
            return False
        return self.driver.is_visible_scrolled("nostr-zap-signer-add-btn")

    def wait_for_zap_signer_section(self, timeout: float = 10.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.is_zap_signer_section_visible():
                return True
            time.sleep(0.3)
        return self.is_zap_signer_section_visible()

    def zap_signer_count(self) -> int:
        return self.driver.count("nostr-zap-signer-item")

    def zap_signer_texts(self) -> list[str]:
        return [
            self.driver.get_text("nostr-zap-signer-item", index=i) or ""
            for i in range(self.zap_signer_count())
        ]

    def add_zap_signer(self, pubkey: str, label: str = "") -> None:
        """Designate a signer: type the 64-hex pubkey
        (`nostr-zap-signer-pubkey-input`) and an optional label
        (`nostr-zap-signer-label-input`), submit (`nostr-zap-signer-add-btn`)."""
        self.driver.wait_for("nostr-zap-signer-pubkey-input", timeout=10.0)
        self.driver.clear_and_type("nostr-zap-signer-pubkey-input", pubkey)
        if label:
            self.driver.clear_and_type("nostr-zap-signer-label-input", label)
        self.driver.click("nostr-zap-signer-add-btn")
        time.sleep(0.5)

    def remove_zap_signer(self, index: int = 0) -> None:
        self.driver.click("nostr-zap-signer-remove", index=index)
        time.sleep(0.5)

    def clear_zap_signers(self, timeout: float = 12.0) -> None:
        """Undesignate every signer so the roster starts from a known-empty
        state (the session nest is shared)."""
        deadline = time.monotonic() + timeout
        while self.zap_signer_count() > 0 and time.monotonic() < deadline:
            self.remove_zap_signer(0)
            self.wait_for_zap_signer_count(self.zap_signer_count() - 1, timeout=4.0)
        remaining = self.zap_signer_count()
        if remaining:
            raise AssertionError(
                f"clear_zap_signers: {remaining} signer(s) still present "
                f"after {timeout}s — the roster is not known-empty; "
                f"{self.driver.diagnose('nostr-zap-signer-item')}"
            )

    def wait_for_zap_signer_count(self, expected: int, timeout: float = 12.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.zap_signer_count() == expected:
                return True
            time.sleep(0.3)
        return self.zap_signer_count() == expected

    def is_zap_signer_empty_state_visible(self) -> bool:
        """Is the "you believe no zap receipt at all" line rendered
        (`nostr-zap-signer-empty`)? A meaningful state, not a loading gap."""
        if self.driver.count("nostr-zap-signer-empty") == 0:
            return False
        return self.driver.is_visible_scrolled("nostr-zap-signer-empty")

    def wait_for_zap_signer_empty_state(self, present: bool = True,
                                        timeout: float = 12.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.is_zap_signer_empty_state_visible() == present:
                return True
            time.sleep(0.3)
        return self.is_zap_signer_empty_state_visible() == present

    # --- Connected apps (NIP-46 bunker) ---
    #
    # The *Connected apps* section (`nostr-bunker-*`, nostr.md § The nest as the
    # user's NIP-46 signer) renders only for a linked custodial account
    # (`generated`/`imported`). Minting an invite (`nostr-bunker-connect-btn`)
    # reveals the one-time `bunker://…` connect string and creates a pending
    # roster row; revoke (`nostr-bunker-app-revoke`) drops it. All four verbs
    # ride `fauna.nostr.bunker.{create_invite,list,revoke,set_label}`.

    def is_connect_section_visible(self) -> bool:
        """True when the Connected apps section (its mint button) renders.

        ``is_visible_scrolled``, not a bare ``is_visible``: this section is the LAST
        one on the page (nostr.md § Layout & flow item 6), so on a client whose
        window is shorter than the page it is rendered but below the fold — windows'
        ``is_visible`` reads UIA ``IsOffscreen`` and reported False for a section
        that was present and correct (``count=1, text='Connect an app'``). Scrolling
        first is what separates *"the app never rendered it"* from *"it is one scroll
        away"*.

        The ``count`` guard is NOT redundant: this predicate's main caller is a
        NEGATIVE assertion (an unlinked account must show no section), and asking a
        driver to scroll to an element that does not exist is what the web bridge
        chokes on — ``POST /element/scroll-into-view`` timed out and took the bridge
        down with it. Absent is answerable without scrolling, so answer it that way;
        only a present element is worth a scroll.
        """
        if self.driver.count("nostr-bunker-connect-btn") == 0:
            return False
        return self.driver.is_visible_scrolled("nostr-bunker-connect-btn")

    def wait_for_connect_section(self, timeout: float = 10.0) -> bool:
        """Poll until the Connected apps section renders (custodial link →
        refresh is async; the section is gated on the linked custodial mode)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.is_connect_section_visible():
                return True
            time.sleep(0.3)
        return self.is_connect_section_visible()

    def connect_app(self) -> None:
        """Mint a connect invite (`nostr-bunker-connect-btn` → create_invite →
        one-time reveal + a pending roster row; async — poll the outcome)."""
        self.driver.wait_for("nostr-bunker-connect-btn", timeout=10.0)
        self.driver.click("nostr-bunker-connect-btn")
        time.sleep(0.5)

    def connect_string(self) -> str:
        """The revealed `bunker://…` connect string (`nostr-bunker-connect-string`)."""
        return self.driver.get_text("nostr-bunker-connect-string") or ""

    def _connect_string_visible(self) -> bool:
        """Scrolled visibility of the reveal, with the same absent-element guard as
        ``is_connect_section_visible`` — doubly load-bearing here, because this runs
        in a POLL: until the mint lands the element does not exist, so an unguarded
        version would ask the bridge to scroll to a missing element on every tick."""
        if self.driver.count("nostr-bunker-connect-string") == 0:
            return False
        return self.driver.is_visible_scrolled("nostr-bunker-connect-string")

    def wait_for_connect_string(self, timeout: float = 12.0) -> bool:
        """Poll until the one-time connect string is revealed. Scrolled, for the
        same reason as ``is_connect_section_visible`` — the reveal appears below the
        mint button, i.e. even deeper past a short window's fold."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self._connect_string_visible():
                return True
            time.sleep(0.3)
        return self._connect_string_visible()

    # ⚠ The connections live on the Connected apps page as signer-client rows
    # (`ui/connected-apps.md` — a lift, never a duplication, on all 7 apps); the
    # invite start stays on this page. So the roster helpers read and revoke on
    # Connected apps, then come back here, where the next step of a journey (the
    # mint button, the reveal) lives.
    def _signer_rows(self):
        from actions.connected_apps import ConnectedAppsActions
        from i18n.strings import S

        ca = ConnectedAppsActions(self.driver)
        # A visit paints no row until its own read is back, so wait for it —
        # an instant count before then would read an empty roster.
        assert ca.visit(), (
            "the Connected apps page never finished its read; "
            f"error: {ca.current_error_text()!r}"
        )
        badge = S.connected_apps.class_signer
        rows = [i for i in range(ca.item_count()) if badge in ca.item_text(i)]
        return ca, rows

    def bunker_app_count(self) -> int:
        _, rows = self._signer_rows()
        self.navigate()
        return len(rows)

    def wait_for_bunker_app_count(self, expected: int, timeout: float = 12.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.bunker_app_count() == expected:
                return True
            time.sleep(0.3)
        return self.bunker_app_count() == expected

    def disconnect_app(self, index: int = 0) -> None:
        """Revoke one connection (the signer row's `connected-apps-item-revoke` → confirm → refresh)."""
        ca, rows = self._signer_rows()
        ca.revoke_item(rows[index])
        self.navigate()

    def clear_bunker_apps(self, timeout: float = 12.0) -> None:
        """Revoke every connection so the roster starts from a known-empty state
        (the session nest is shared; the per-account cap is a hard constant)."""
        deadline = time.monotonic() + timeout
        while self.bunker_app_count() > 0 and time.monotonic() < deadline:
            self.disconnect_app(0)
            self.wait_for_bunker_app_count(self.bunker_app_count() - 1, timeout=4.0)
        remaining = self.bunker_app_count()
        if remaining:
            raise AssertionError(
                f"clear_bunker_apps: {remaining} connection(s) still present "
                f"after {timeout}s — the roster is not known-empty; "
                f"{self.driver.diagnose('connected-apps-item')}"
            )

    # --- Succession-aftermath npub confirm (leg 3 — nostr.md § Key succession
    # and rotation) ---

    def is_npub_confirm_visible(self) -> bool:
        """True while the successor is owed a confirmation of the linked npub.

        Dismissible, never a blocking modal — absence, not emptiness, is the
        un-raised state (the same convention the destination/trust/member
        marks take in `test_identity_succession_aftermath.py`)."""
        return not self.driver.is_absent("nostr-npub-confirm-banner")

    def wait_for_npub_confirm_visible(self, timeout: float = 12.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.is_npub_confirm_visible():
                return True
            time.sleep(0.3)
        return self.is_npub_confirm_visible()

    def wait_for_npub_confirm_gone(self, timeout: float = 12.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self.is_npub_confirm_visible():
                return True
            time.sleep(0.3)
        return not self.is_npub_confirm_visible()

    def confirm_npub(self) -> None:
        """`nostr-npub-confirm-yes-button` — writes the confirm timestamp,
        then a non-optimistic refresh (the banner's disappearance is a fresh
        read, not an assumed outcome)."""
        self.driver.click("nostr-npub-confirm-yes-button")

    def dismiss_npub_to_new_key(self) -> None:
        """`nostr-npub-confirm-no-button` — routes into the EXISTING new-key
        path (unlink, then re-link via the existing form) rather than a
        bespoke one (`nostr.md`:75)."""
        self.driver.click("nostr-npub-confirm-no-button")

    # --- Errors ---

    def page_error_text(self, timeout: float = 8.0) -> str:
        """Return the page's error message once it appears, else "".

        Reads the state protocol's `messages.error` first (the canonical
        cross-app error surface), falling back to the `error-message`
        element (`ErrorBanner`, NostrSettingsView). Mirrors the shared
        `App.error_text()` / linked-nests contract.
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
                    return txt
            time.sleep(0.3)
        return ""
