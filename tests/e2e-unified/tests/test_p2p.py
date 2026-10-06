"""P2P (Direct Contacts) settings page (docs/goal/behavior/p2p.md; ui.yaml
`p2p` page).

tier_3 (full stack — real `fauna-nest` binary + real client driver): driving
the real Start/Stop control is what proves the button, the async round-trip,
and the row-state gating on the copy buttons actually wire together.

**linux only.** Per p2p.md § Implementation status / § Element IDs: web is a
settings redirect stub (no dedicated surface, no elements), and windows, tui,
macOS/iOS and android have no page surface at all. linux is the only app with
the page today.

That is a NARROWING, and a deliberate one: until 2026-08-23 tui and windows
each shipped a P2P surface, and both were WireGuard peer-registration forms —
`wg-pubkey-input` / `wg-device-name` / `wg-listen-port` / `wg-register-button`
/ `wg-unregister-button` behind the `p2p-wireguard-form` component. The whole
WireGuard stack was deleted that day (user-directed; iroh-QUIC is the only
substrate), and those forms went with it. What this file lost with them:

* `test_tui_peer_registration_round_trip` and its `wireguard_app` fixture —
  the fixture existed only because a WG-enabled nest needed `--features
  wireguard` AND an enabled `[wireguard]` config table, neither of which the
  shared session nest had. No nest-side registration surface remains, so the
  test has nothing to round-trip and the isolated nest has nothing to enable.
* `test_stun_copy_is_noop` — `p2p-stun-copy-btn` is gone. The row was fed by
  the nest's STUN server, which was part of the WireGuard stack; with the
  server deleted, nothing could ever populate it, so the element was retired
  rather than left as a permanently-inert affordance to assert.
* the two-model branch in `actions/p2p.py` — with tui gone there is only one
  model left (a local Start/Stop toggle), so `P2PActions` no longer forks on
  `driver.is_tui()`.

`p2p-lan-copy-btn` deliberately SURVIVED that sweep: the LAN arithmetic moved
into `fauna-peer-sync::lan` rather than being deleted, and linux still renders
the row, so retiring its id would have dropped coverage of a working control.

**Starting the tunnel never reaches external network egress.** linux never
configures a relay URL, so the iroh endpoint binds with `RelayMode::Disabled`.

(`p2p-invite-copy-btn` and the whole pairing surface it fronted were retired
2026-08-18, user-approved — p2p.md § No pairing step, ever. The element no
longer exists in ui.yaml, and `test_invite_copy_is_noop` retired with it.)
"""
import pytest

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


@pytest.mark.linux
@pytest.mark.feature("direct-device-connections")
def test_p2p_page_renders(logged_in_app):
    """The P2P page is reachable from user settings, and its
    always-rendered controls (the Start/Stop toggle and both copy buttons)
    are present regardless of tunnel state."""
    app = logged_in_app
    app.p2p.navigate()
    assert app.p2p.is_page_visible(), (
        f"p2p page not reachable. error: {app.error_text()!r}"
    )
    assert app.driver.is_visible("p2p-tunnel-toggle"), (
        f"p2p-tunnel-toggle should render on the p2p page: "
        f"{app.driver.diagnose('p2p-tunnel-toggle')}"
    )
    for copy_btn_id in app.p2p.COPY_BUTTONS:
        assert app.driver.is_visible(copy_btn_id), (
            f"{copy_btn_id} should render on the p2p page: {app.driver.diagnose(copy_btn_id)}"
        )


@pytest.mark.linux
@pytest.mark.feature("direct-device-connections")
def test_tunnel_start_stop_round_trip(logged_in_app):
    """Start brings up a local iroh PeerNode (button flips to Stop, the
    node-id copy button goes from no-op to live); Stop tears it down (button
    flips back to Start). Exercises `P2pService::{start,stop}_tunnel`
    end-to-end through the real UI control, not just the Rust unit level."""
    app = logged_in_app
    app.p2p.navigate()
    assert app.p2p.is_page_visible(), (
        f"p2p page should be reachable from user settings. error: {app.error_text()!r}"
    )
    # Deterministic start state (a shared-nest app may be reused across tests).
    assert app.p2p.ensure_tunnel_inactive(), (
        f"could not reach the inactive baseline. error: {app.error_text()!r}"
    )
    # Before Start, the node-id row has nothing to copy yet.
    app.p2p.assert_copy_state("p2p-node-id-copy-btn", should_be_live=False)

    app.p2p.start_tunnel()
    assert app.p2p.wait_for_tunnel_active(), (
        f"start should bring the tunnel up. error: {app.error_text()!r}"
    )

    # Once active, the node's own identity (its iroh NodeId — the hex of this
    # actor's Ed25519 public key) populates the Node ID row, so the copy
    # button stops being a no-op.
    app.p2p.assert_copy_state("p2p-node-id-copy-btn", should_be_live=True)

    app.p2p.stop_tunnel()
    assert app.p2p.wait_for_tunnel_inactive(), (
        f"stop should tear the node down. error: {app.error_text()!r}"
    )
    # ...and the row reverts to its placeholder, so the button is inert again.
    app.p2p.assert_copy_state("p2p-node-id-copy-btn", should_be_live=False)


@pytest.mark.linux
@pytest.mark.feature("direct-device-connections")
def test_start_failure_renders_error_message(logged_in_app):
    """A failed Start renders `error-message` — the
    page previously wrote the failure into the untagged status-row subtitle,
    where e2e convention 2's read-the-error-first rule had nothing to read.

    Drives the REAL bind failure through the real button (convention 8's
    carve-out (b)): `p2p.force_bind_conflict()` arranges the precondition (a
    held-open loopback socket at the exact address `start_tunnel` will try
    next), then the click is what actually fails. Also proves the negative
    side of convention 2's rider — no error before the failure, and the
    element clears (goes absent) on the next successful start."""
    app = logged_in_app
    app.p2p.navigate()
    assert app.p2p.is_page_visible(), (
        f"p2p page should be reachable from user settings. error: {app.error_text()!r}"
    )
    assert app.p2p.ensure_tunnel_inactive(), (
        f"could not reach the inactive baseline. error: {app.error_text()!r}"
    )
    assert not app.has_error(), (
        f"no error should be visible before anything has failed: {app.error_text()!r}"
    )

    app.p2p.force_bind_conflict()
    try:
        app.p2p.start_tunnel()
        assert app.p2p.wait_for_error(), (
            f"a bind conflict should surface as {app.error_text()!r}"
        )
        assert app.error_text(), "error-message should carry the failure text, not be empty"
        # The failed attempt must not leave the tunnel looking active.
        assert not app.p2p.is_tunnel_active(), (
            "a failed start should leave the toggle reading Start, not Stop"
        )
    finally:
        app.p2p.clear_bind_conflict()

    # Retrying now succeeds, and the stale error clears — the rider's
    # "cleared on success" half, and proof the read sees the REAL current
    # state, not a leftover from the previous attempt.
    app.p2p.start_tunnel()
    assert app.p2p.wait_for_tunnel_active(), (
        f"the retry should succeed once the conflict is cleared. error: {app.error_text()!r}"
    )
    assert not app.has_error(), (
        f"error-message should go absent once the retry succeeds: {app.error_text()!r}"
    )

    app.p2p.stop_tunnel()
    assert app.p2p.wait_for_tunnel_inactive(), (
        f"cleanup stop should tear the node down. error: {app.error_text()!r}"
    )


@pytest.mark.linux
@pytest.mark.feature("direct-device-connections")
def test_lan_addresses_copy_round_trip(logged_in_app):
    """LAN addresses are discovered synchronously from local network
    interfaces (no tunnel required) via the shared `fauna_peer_sync::lan`
    arithmetic, and copy successfully whenever this device has a real
    RFC-1918 address — which every dev machine and CI runner in the fleet
    does, but a bare loopback-only sandbox would not, so the assertion
    follows what the host itself reports rather than assuming."""
    app = logged_in_app
    app.p2p.navigate()
    assert app.p2p.is_page_visible(), (
        f"p2p page should be reachable from user settings. error: {app.error_text()!r}"
    )
    app.p2p.assert_copy_state(
        "p2p-lan-copy-btn", should_be_live=_host_has_private_ipv4()
    )


def _host_has_private_ipv4() -> bool:
    """Whether THIS machine (running the linux app under test) has at least
    one private IPv4 address — the same fact `p2p-lan-copy-btn` now reports.
    No packet leaves the host: connecting a UDP socket only resolves the
    local route the kernel would use, matching the well-known
    "no-traffic-outbound-IP" trick."""
    import ipaddress
    import socket

    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
        try:
            probe.connect(("10.255.255.255", 1))
            local_ip = probe.getsockname()[0]
        except OSError:
            return False
    try:
        return ipaddress.ip_address(local_ip).is_private
    except ValueError:
        return False


@pytest.mark.linux
@pytest.mark.feature("direct-device-connections")
def test_p2p_contact_list_renders_and_removes(logged_in_app):
    """apps row 128 / p2p.md § Implementation status today: `PeerDb::delete_contact`
    was written, tested, and exported over FFI, but no app rendered a list
    to hang a removal button on — linux's since-retired Accept-Invite dialog
    could ADD a contact with no way to see or remove it afterward. Seeds two
    contacts via the `p2p_seed_contact_for_test` fixture command (the add
    flow is retired, p2p.md § No pairing step, ever — e2e-conventions point
    8: fixture setup, not the mutation under test), then drives the REAL
    `p2p-contact-remove-button` click. linux only: it is the only app that
    renders the list today; the list trickles to the other apps with the
    trigger-time contacts-plane-derived opt-in (same p2p.md §)."""
    app = logged_in_app
    app.p2p.navigate()
    assert app.p2p.is_page_visible(), (
        f"p2p page should be reachable from user settings. error: {app.error_text()!r}"
    )
    before = app.p2p.contact_count()

    app.p2p.seed_contact("11" * 32, "Alice Peer")
    app.p2p.seed_contact("22" * 32, "Bob Peer")
    app.p2p.navigate()  # re-nav triggers the refresh (main.rs's nav match)
    assert app.p2p.contact_count() == before + 2, (
        f"seeding 2 contacts should render 2 new p2p-contact-row entries. "
        f"error: {app.error_text()!r}"
    )
    names = {app.p2p.contact_name(i) for i in range(app.p2p.contact_count())}
    assert {"Alice Peer", "Bob Peer"} <= names, (
        f"seeded contact names should render, got {names!r}"
    )

    # Remove one by name — the real button, the mutation under test.
    alice_index = next(
        i for i in range(app.p2p.contact_count())
        if app.p2p.contact_name(i) == "Alice Peer"
    )
    app.p2p.remove_contact(alice_index)
    assert app.p2p.contact_count() == before + 1, (
        f"removal should drop exactly one row. error: {app.error_text()!r}"
    )
    remaining = {app.p2p.contact_name(i) for i in range(app.p2p.contact_count())}
    assert "Alice Peer" not in remaining, "the removed contact should no longer render"
    assert "Bob Peer" in remaining, "removal should not touch the other contact"

    # Cleanup — leave the shared session app as this test found it (linux's
    # app process is not relaunched between tests within this module).
    bob_index = next(
        i for i in range(app.p2p.contact_count())
        if app.p2p.contact_name(i) == "Bob Peer"
    )
    app.p2p.remove_contact(bob_index)
    assert app.p2p.contact_count() == before, "cleanup should restore the baseline count"
