"""Tier-3 verification of the user-class ``fauna.sync.register`` WS-RPC helper.

Proves the ``sync_register`` helper (``tests/common/auth.py``) end-to-end against
a real ``fauna-nest`` binary — AND that the user-class WS-RPC challenge/verify
path works for an actor admitted through the **registration ceremony**. That is
the auth shape the darwin-only ``tests/platform/macos/test_file_sync.py`` uses, so
a green run here de-risks that test on a machine where it cannot run.

Formerly this pinned a *token-minted-but-never-registered* actor on a
``--no-require-registration`` nest. **That actor cannot exist any more**: the
handshake's auto-provision branch is deleted, so a valid self-signed token proves
key possession, never admission (``public-mode.md`` § User Registration —
registering *is* choosing a handle; ``login.md`` § Errors). The nest now opens
registration via the ``[nest] registration_mode`` seed and the actor arrives
through ``fauna.account.register``, exactly as a real client does.

This covers only the control-plane setup the macOS test shares — registering a
device, creating a folder, and listing changes over the ``fauna.sync.*`` /
``fauna.folders.*`` kinds. The FSEvents-driven upload assertion stays macOS-only
in ``macos/test_file_sync.py``.
"""

import os

import pytest

from common import (
    sync_changes_list,
    sync_devices_list,
    sync_register,
    user_create_folder,
)
from common.auth import register_handled_actor
from fauna_ffi import open_device_label

pytestmark = pytest.mark.tier_3

DOMAIN = "test.fauna.social"


@pytest.fixture(scope="function")
def sync_register_nest(request, nest_mode, tmp_path_factory):
    """A fresh nest claimed onto ``DOMAIN`` with self-service registration open.

    Both tests here used to spawn this themselves — a raw ``start_nest`` call on
    a FIXED port, twice, one apart. That made them the mode axis's blindest
    shape: a locally-compiled binary arriving as a test PARAMETER, invisible to
    both AST pins because there was no fixture to grade (``testing.md`` § Default
    app and nest mode, ruling (1) — every nest the harness starts is the mode
    provider's to start). Routing them minted this fixture rather than rewriting
    one, and the fixed ports went with the raw spawn: allocation is the
    provider's business now.

    ``claim_domain`` is a wire act every mode honours, and registration is opened
    AFTER boot over the admin kind (ruling (3)) — so this fixture asks the
    provider for nothing standalone-only and collects in a container.

    Function-scoped: the second test wants a nest no ceremony has run against,
    which is what the two separately-spawned nests were buying.
    """
    from common.auth import open_registration
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "sync-register-nest",
        claim_domain=DOMAIN,
    )
    try:
        open_registration(nest)
        yield nest
    finally:
        cleanup()


def test_sync_register_ceremony_registered_actor(sync_register_nest):
    """An actor admitted through ``fauna.account.register`` registers a sync
    device, creates a folder, and lists changes — all over the user-class
    ``fauna.sync.*`` / ``fauna.folders.*`` WS-RPC kinds. This is the macOS
    test's exact auth + control-plane shape, sans FSEvents."""
    nest = sync_register_nest
    port = nest["port"]
    actor = register_handled_actor(port, handle="syncuser", domain=DOMAIN)
    secret_hex = bytes(actor["signing_key"]).hex()
    device_id = os.urandom(32).hex()

    # The migration's net-new helper: register the device.
    reply = sync_register(
        port, secret_key=secret_hex, device_id=device_id, label="e2e-verify"
    )
    assert reply.get("device_id") == device_id, (
        f"fauna.sync.register should echo the stored device id; got {reply!r}"
    )

    # The helper's post-S9-flip contract: it seals the label through the real
    # funnel, so the row the owner lists back RENDERS the name they chose.
    # Asserting the render (not the `label` column) is the whole point — the
    # nest scrubs the plaintext for a user-chosen label, and this plane's
    # degrade is the WEAK one: an unsealed register still returns a row, still
    # counts, and only reads nameless. So a seam that silently stopped sealing
    # would look exactly like success everywhere except here.
    listed = sync_devices_list(port, secret_key=secret_hex)
    row = next(
        (d for d in listed.get("devices", []) if d.get("device_id") == device_id),
        None,
    )
    assert row is not None, f"registered device missing from devices.list: {listed!r}"
    assert row.get("label_sealed"), (
        f"sync_register must seal the label — the row came back sealless: {row!r}"
    )
    assert open_device_label(
        bytes.fromhex(secret_hex),
        bytes.fromhex(device_id),
        row.get("label_sealed"),
        row.get("label", "") or "",
    ) == "e2e-verify", f"the sealed label did not render back: {row!r}"

    # The other two twins the macOS test swaps to WS-RPC helpers.
    user_create_folder(port, "test-sync", secret_key=secret_hex)
    changes = sync_changes_list(
        port, secret_key=secret_hex, folder="test-sync"
    )
    assert changes.get("changes") == [], (
        f"a fresh folder lists no changes; got {changes!r}"
    )


def test_handshake_refuses_an_unregistered_actor(sync_register_nest):
    """The removal, pinned at the binary level: on a real nest with registration
    wide **open**, an actor that never ran the ceremony still cannot mint a token.

    `Open` is the sharp end — even a nest that admits anyone admits them through
    `fauna.account.register`, never through a bare handshake. This is the e2e twin
    of `conformance_auth::handshake_refuses_an_unregistered_actor_in_every_mode`.
    """
    from nacl.signing import SigningKey

    from common.auth import mint_token_via_handshake

    nest = sync_register_nest
    with pytest.raises(Exception) as excinfo:
        mint_token_via_handshake(nest["url"], SigningKey.generate())
    assert "not_registered" in str(excinfo.value), (
        "an unregistered actor must be refused a token even on an open nest; "
        f"got {excinfo.value!r}"
    )
