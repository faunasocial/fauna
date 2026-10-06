from __future__ import annotations

import time
from typing import TYPE_CHECKING

from i18n.strings import S

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class P2PActions:
    """Drive the standalone **P2P** (Direct Contacts) settings page
    (docs/goal/behavior/p2p.md; ui.yaml `p2p` page).

    **The peer data plane is dormant fleet-wide** (p2p.md § Implementation
    status today) — this page brings up a local iroh `PeerNode` (the substrate
    behind the `fauna-transport` seam) but no live chunk/manifest traffic rides
    it yet.

    **linux only.** `apps/fauna-linux/src/settings/p2p_tab.rs` is the sole
    implementation: web is a settings redirect stub, and windows, tui,
    macOS/iOS and android have no page surface at all (p2p.md § Element IDs
    matrix). tui and windows DID ship one until 2026-08-23 — both were
    WireGuard peer-registration forms, and both went with the stack
    (user-directed; iroh-QUIC is the only substrate). What that removed from
    this class was the whole two-model branch: there is no
    `driver.is_tui()` fork left here, because there is no second model.

    The surviving surface is four ids plus the contact list:
    `p2p-tab`, `p2p-tunnel-toggle` (one button whose label flips Start/Stop),
    `p2p-node-id-copy-btn`, `p2p-lan-copy-btn`.

    **Starting the tunnel is local-only, no network egress.** linux never
    configures a relay URL (`P2pService::start_tunnel`,
    `apps/fauna-linux/src/p2p.rs`), so `IrohTransport::builder(...).build()`
    binds a local UDP endpoint with `RelayMode::Disabled` — it does not dial
    out or depend on internet connectivity. Safe and fast to drive in e2e.

    (The invite affordance and the whole pairing surface it fronted were
    retired 2026-08-18, user-approved — p2p.md § No pairing step, ever;
    `p2p-invite-copy-btn` no longer exists. `p2p-stun-copy-btn` went
    2026-08-23: the nest's STUN server was part of the WireGuard stack, so
    nothing could ever populate that row again.)
    """

    # The 2 copy-affordance buttons (ui.yaml `p2p` elements, minus the tab,
    # the toggle, the contact-list ids and shared `error-message`).
    COPY_BUTTONS = (
        "p2p-node-id-copy-btn",
        "p2p-lan-copy-btn",
    )

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    # --- Navigation ---

    def navigate(self) -> None:
        """Navigate to the P2P settings sub-page.

        The cross-app two-element nav stack
        `{"view":"settings"},{"view":"settings","id":"p2p"}` selects the
        "p2p" rail sub-page on linux's sidebar-swap settings shell
        (`settings_shell.rs` — same pattern as `mail`/`nostr`).
        """
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "p2p"}]},
        })

    def is_page_visible(self, timeout: float = 10.0) -> bool:
        """True once the P2P page is reachable.

        Anchored on `p2p-tunnel-toggle`: it renders unconditionally, unlike
        the tunnel-state-gated rows. (It used to anchor on `wg-device-name`,
        which went with the WireGuard form.)
        """
        try:
            self.driver.wait_for("p2p-tunnel-toggle", timeout=timeout)
            return True
        except TimeoutError:
            return False

    # --- Tunnel lifecycle ---
    #
    # `p2p-tunnel-toggle` is ONE button whose label flips Start/Stop
    # (p2p_tab.rs). There is no registration ceremony and no nest round trip:
    # starting binds a local-only iroh node, stopping drops it.

    def toggle_button_label(self) -> str:
        return self.driver.get_text("p2p-tunnel-toggle")

    def is_tunnel_active(self) -> bool:
        return self.toggle_button_label() == S.settings.p2p_page.stop

    def start_tunnel(self) -> None:
        """Click Start. Binds a local-only iroh node (no relay configured),
        so it resolves fast with no external network egress."""
        self.driver.wait_for("p2p-tunnel-toggle", timeout=10.0)
        self.driver.click("p2p-tunnel-toggle")

    def stop_tunnel(self) -> None:
        """Click Stop — `P2pService::stop_tunnel()` is synchronous (it drops
        the node, aborting its accept loop)."""
        self.driver.wait_for("p2p-tunnel-toggle", timeout=10.0)
        self.driver.click("p2p-tunnel-toggle")

    def wait_for_tunnel_active(self, timeout: float = 10.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.is_tunnel_active():
                return True
            time.sleep(0.2)
        return self.is_tunnel_active()

    def wait_for_tunnel_inactive(self, timeout: float = 10.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self.is_tunnel_active():
                return True
            time.sleep(0.2)
        return not self.is_tunnel_active()

    def ensure_tunnel_active(self) -> bool:
        if self.is_tunnel_active():
            return True
        self.start_tunnel()
        return self.wait_for_tunnel_active()

    def ensure_tunnel_inactive(self) -> bool:
        if not self.is_tunnel_active():
            return True
        self.stop_tunnel()
        return self.wait_for_tunnel_inactive()

    def wait_for_error(self, timeout: float = 10.0) -> bool:
        """Poll until a page error is present, returning whether one appeared.

        `start_tunnel` runs the actual bind on an async glib task, so the
        error (a failed `P2pService::start_tunnel`) appears a beat after the
        click. Presence is read the same way `ActionLayer.error_text`
        resolves it — state first (`messages.error`), the `error-message`
        element only as a fallback — though linux publishes no `messages`
        state today, so this always falls through to the element read."""
        deadline = time.monotonic() + timeout
        while True:
            if self._error_present():
                return True
            if time.monotonic() >= deadline:
                return self._error_present()
            time.sleep(0.2)

    def _error_present(self) -> bool:
        err = self.driver.get_state("messages.error")
        if err is not None:
            return bool(err)
        if self.driver.get_state("messages") is not None:
            return False
        return self.driver.is_visible("error-message")

    # --- Copy buttons ---
    #
    # Every copy button shares one behavior (p2p_tab.rs): click sets its own
    # label to `S.settings.account_page.copied_clipboard` for ~2s, then
    # reverts to `S.p2p.copy_to_clipboard` — but ONLY when the row's
    # underlying value is populated (not a "(no active interfaces detected)"
    # or "—" placeholder). A click against a placeholder is a documented
    # no-op: the label never changes. The button itself stays always-enabled;
    # liveness is expressed purely through this label transition.

    def copy_button_label(self, copy_btn_id: str) -> str:
        return self.driver.get_text(copy_btn_id)

    def click_copy(self, copy_btn_id: str) -> None:
        self.driver.wait_for(copy_btn_id, timeout=10.0)
        self.driver.click(copy_btn_id)

    def wait_for_copied(self, copy_btn_id: str, timeout: float = 3.0) -> bool:
        """Poll until `copy_btn_id`'s label shows the transient "copied"
        state. Returns False (not a timeout raise) so callers can also assert
        the no-op case."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.copy_button_label(copy_btn_id) == S.settings.account_page.copied_clipboard:
                return True
            time.sleep(0.1)
        return self.copy_button_label(copy_btn_id) == S.settings.account_page.copied_clipboard

    def assert_copy_is_noop(self, copy_btn_id: str, timeout: float = 1.5) -> bool:
        """True if clicking `copy_btn_id` leaves its label unchanged for
        `timeout` — the placeholder-value no-op case."""
        before = self.copy_button_label(copy_btn_id)
        self.click_copy(copy_btn_id)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.copy_button_label(copy_btn_id) != before:
                return False
            time.sleep(0.1)
        return self.copy_button_label(copy_btn_id) == before

    def assert_copy_state(self, copy_btn_id: str, *, should_be_live: bool) -> None:
        """Liveness assertion for `copy_btn_id` — the call site every test in
        `test_p2p.py` should use (e2e-conventions point 3: the signal lives in
        the action layer, never the test file)."""
        if should_be_live:
            self.click_copy(copy_btn_id)
            assert self.wait_for_copied(copy_btn_id), (
                f"{copy_btn_id} should go live (label should flip to the "
                f"'copied' state)"
            )
        else:
            assert self.assert_copy_is_noop(copy_btn_id), (
                f"{copy_btn_id} should stay a no-op"
            )

    # --- Contact list + removal (linux only today — apps row 128) ---
    #
    # `PeerDb::delete_contact` was written, tested, and exported over FFI, but
    # no app rendered a list to hang a removal button on — linux's Accept
    # Invite dialog could ADD a contact with no way to see or remove it
    # afterward. That dialog has no test ids of its own (a manual-fields
    # `adw::MessageDialog`), so fixture setup goes through the
    # `p2p_seed_contact_for_test` command instead (fixture setup, not the
    # mutation under test — e2e-conventions point 8); the actual removal
    # test drives the real `p2p-contact-remove-button` click.

    def seed_contact(self, actor_id_hex: str, display_name: str) -> None:
        """Seed a P2P contact directly on the local PeerDb via the
        `p2p_seed_contact_for_test` command — linux only (`handle_p2p_seed_contact_for_test`,
        `apps/fauna-linux/src/main.rs`). `actor_id_hex` must be exactly 64 hex
        chars (32 bytes)."""
        self.driver.call_command(
            "p2p_seed_contact_for_test",
            {"actor_id_hex": actor_id_hex, "display_name": display_name},
        )

    # --- Forced bind-conflict fixture ---
    #
    # `P2pService::start_tunnel` has no start-failure edge a real click can
    # reach on its own (the already-active guard is unreachable via the
    # toggle, and the OS-assigned bind essentially never fails). This command
    # arranges a REAL precondition instead of simulating the error
    # (e2e-conventions point 8's carve-out (b)): it binds and holds open a
    # loopback UDP socket, then points the next `start_tunnel` call's iroh
    # bind at that same address, so the click drives an actual bind-conflict
    # failure.

    def force_bind_conflict(self) -> None:
        self.driver.call_command("p2p_force_bind_conflict_for_test", {})

    def clear_bind_conflict(self) -> None:
        self.driver.call_command("p2p_clear_bind_conflict_for_test", {})

    def contact_count(self) -> int:
        return self.driver.count("p2p-contact-row")

    def contact_name(self, index: int = 0) -> str:
        return self.driver.get_text("p2p-contact-name", index=index)

    def remove_contact(self, index: int = 0) -> None:
        self.driver.wait_for("p2p-contact-remove-button", timeout=10.0)
        self.driver.click("p2p-contact-remove-button", index=index)
