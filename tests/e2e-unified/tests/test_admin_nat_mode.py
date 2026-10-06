"""admin-nest NAT-mode control (admin.md § Nest → NAT-mode control) — the
post-onboarding change surface for the nest's NAT axis (`public`/`private`),
the axis the wizard's `nat_mode_choice` page confirms once at claim.

The control is driven by the shared `AdminNatModeMachine`
(`fauna-onboarding-machine::admin_nat_mode` — the exact seam + signed commit
ceremony of the wizard page, exposed over UniFFI for native apps and the
wasm `AdminNatModeMachine` twin for web): hydrate pre-selects the current
`fauna.setup.status` `node_mode`; save signs the canonical payload and commits
via the **mutable** `fauna.setup.nat_mode` (the Ed25519 admin payload
signature is the authorization — no bearer; the nest upserts the
`nest_nat_mode` row and live-re-evaluates the MTA supervisor + MDA bind).

tier_3: a real client driver against a real `fauna-nest` binary. Ground truth
is the `fauna.setup.status` `node_mode` read — the same read the control
hydrates from — not UI introspection alone.

Client markers: linux + web + android (android's `AdminNestScreen.kt` render
landed 2026-07-13, `libs/fauna-onboarding-machine/src/admin_nat_mode.rs`'s
second UniFFI consumer; emulator-gated for the actual run, blocked on the
Android emulator's dev-machine dependency in the meantime — compile-verified
+ Robolectric-covered instead) + macos + ios (shared FaunaKit `AdminNestVM`/
`AdminNestView` render landed 2026-07-13, the third UniFFI consumer, over a
new `APIClient.adminNatModeMachine()` wrapper — no per-app deviation from
the linux/web radio+save+status shape) + windows (`AdminNestPage.xaml` +
`AdminNatModeViewModel` landed 2026-07-14, the sixth and last UniFFI consumer,
over the `IAdminNatModeMachine` seam constructed from `ISecretStore` —
build-verified via `AdminNatModeViewModelTests`; FlaUI e2e flakes on
win-arm64, same caveat as the rest of this page).
"""
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.android,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.tui,  # NAT-mode radios on the admin-nest sub-page (M8, 2026-07-18)
]


def _admin_client(nest_instance):
    """An Admin WS-RPC client for the ground-truth `fauna.setup.status` read
    (an anonymous kind, callable over any connection — mirrors
    test_serving_port)."""
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _wait(predicate, timeout: float = 15.0, interval: float = 0.3) -> bool:
    """Poll `predicate` until true or the deadline (hydrate + save are async:
    admin nav → setup.status → render; save → sign → commit → re-render)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()


def _node_mode(client) -> str:
    """Ground-truth `node_mode` from `fauna.setup.status` (the DB-resolved
    `nest_nat_mode` row falling back to the seed; the same read the control
    hydrates from). The nest always sends it; the `or "public"` is only a defensive default."""
    return str(client.call("fauna.setup.status", {}).get("node_mode") or "public")


@pytest.mark.feature("admin-nest")
def test_nat_mode_control_renders(admin_app):
    """The admin-nest page renders the NAT-mode control: both radios, the save
    button, and the status line carrying the live-vs-restart caveat."""
    admin_app.admin.navigate_nest()
    assert admin_app.driver.count("admin-nest-heading") > 0, (
        f"admin-nest-heading missing. error: {admin_app.error_text()!r}"
    )
    assert _wait(admin_app.admin.nat_mode_control_present), (
        "admin-nest-nat-mode radios / save button missing. "
        f"error: {admin_app.error_text()!r}"
    )
    assert _wait(lambda: admin_app.driver.count("admin-nest-nat-mode-status") > 0), (
        f"admin-nest-nat-mode-status missing. error: {admin_app.error_text()!r}"
    )


@pytest.mark.feature("admin-nest")
def test_nat_mode_flip_round_trips(admin_app, nest_instance):
    """Flipping the NAT mode from the admin UI commits through the signed
    `fauna.setup.nat_mode` upsert, proven by the `fauna.setup.status`
    `node_mode` ground-truth read; a second flip restores the original mode
    (the set is mutable — no conflict, resubmit always allowed), leaving the
    shared session nest exactly as found."""
    admin_app.admin.navigate_nest()
    assert _wait(admin_app.admin.nat_mode_control_present), (
        f"NAT-mode control did not render. error: {admin_app.error_text()!r}"
    )

    client = _admin_client(nest_instance)
    with client:
        initial = _node_mode(client)
        flipped = "private" if initial == "public" else "public"

        admin_app.admin.select_nat_mode(flipped)
        admin_app.admin.save_nat_mode()

        try:
            assert _wait(lambda: _node_mode(client) == flipped), (
                "the NAT-mode save did not persist "
                f"(setup.status.node_mode still {_node_mode(client)!r}, wanted {flipped!r}). "
                f"status: {admin_app.admin.nat_mode_status_text()!r} "
                f"error: {admin_app.error_text()!r}"
            )
        finally:
            # Restore the original mode — the harness nest is shared and a
            # private-axis leftover would gate the MTA supervisor for every
            # later mail test. The mutable upsert makes this a plain second
            # save from the same UI.
            admin_app.admin.select_nat_mode(initial)
            admin_app.admin.save_nat_mode()
            _wait(lambda: _node_mode(client) == initial)

    assert _node_mode_restored(admin_app, nest_instance, initial)


def _node_mode_restored(admin_app, nest_instance, initial: str) -> bool:
    """Post-restore ground truth (kept out of the `with` so a failed restore
    reads as its own assertion, not a teardown crash)."""
    client = _admin_client(nest_instance)
    with client:
        ok = _node_mode(client) == initial
    if not ok:
        print(
            f"[test_admin_nat_mode] restore failed; status: "
            f"{admin_app.admin.nat_mode_status_text()!r}"
        )
    return ok
