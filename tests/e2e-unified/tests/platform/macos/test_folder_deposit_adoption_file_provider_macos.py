"""tier_3: a third-party deposit adopted by the macOS File Provider host's refresh.

The mac proof of the adoption pass the host's refresh arm runs
(``docs/goal/behavior/on-demand-files-apple.md`` § Apple File Provider binding →
*Third-party deposit adoption on a capability host*, its *Proof* clause;
``docs/goal/behavior/folder-deposit.md`` § Implementation status today):

  1. an owner signs in, makes a folder, and a connected app deposits one file
     over the HTTP door (the consent ceremony and helpers are
     ``tests/api/test_folder_deposit.py``'s);
  2. the app-dead capability is provisioned and a domain registered for the
     folder through the Fauna FP host app, exactly as
     ``test_file_provider_read_path.py`` does;
  3. ``Fauna signal`` nudges the enumerator — ``enumerateChanges`` →
     ``host.refresh()`` — and the deposit is adopted;
  4. from **another process** (``ls`` on ``~/Library/CloudStorage/``) the item is
     listed under its deposited name as a placeholder; the owner's
     ``fauna.folders.deposits.list`` is empty (adoption retired the parked item);
     and neither the file's bytes nor its name rests in clear anywhere on the
     nest's disk.

**Signing-gated, like the read-path test.** ``signal`` reaches the extension that
``fileproviderd`` runs, which reads the shared-group Keychain credential the CLI
provisioned; that handoff needs a signed app with an embedded provisioning
profile (``on-demand-files-apple.md`` § Headless-first testing). Until the profile
lands this test collects and skips; the
in-process mechanism is pinned by the nest conformance tests.

**Consent posture.** The depositor's consent is approved with the raw
``_approve_with_grant`` mint, not on an app's request card: this test runs no app
seat, so no account runtime — the only owner of the reconcile sweep
(``ui/nests.md`` § Trust facet — grants → *Reconcile*) — is live beside the nest, and
nothing in the File Provider host path runs the sweep, so the minted row is never
revoked. A seat joining this test would have to approve through the app as
``test_folder_deposit_adoption.py::_depositor`` does.
"""

import os
import secrets
import sys

import blake3
import pytest

from helpers.principal_session import alice_client
from helpers.recipient_seal_key import provision_recipient_seal_key
from tests.api.test_folder_deposit import (  # noqa: F401 — metadata_server/consent_nest_env are pytest fixtures adopted by import
    BARE_SCOPE,
    HTTP_MARKER,
    HTTP_NAME,
    _approve_with_grant,
    _client,
    _everything_on_disk,
    _http_deposit,
    _inbox,
    consent_bridge,
    consent_nest,
    consent_nest_env,
    metadata_server,
)
from tests.platform.macos.test_file_provider_read_path import (  # noqa: F401 — fauna_fp_host_app is a pytest fixture adopted by import
    _BACKUP_KEY_CONTEXT,
    _CLOUD,
    _has_codesigning_identity,
    _run_host,
    _safe_listdir,
    _wait_for,
    fauna_fp_host_app,
)

pytestmark = [
    pytest.mark.skipif(sys.platform != "darwin", reason="macOS-only: File Provider extension"),
    pytest.mark.skipif(
        not _has_codesigning_identity(),
        reason="appex-driven FP tier_3 is signing-gated: `Fauna signal` reaches the extension, "
        "which needs the app-group data-protection Keychain credential (errSecMissingEntitlement "
        "-34018 without a signed app + embedded provisioning profile). "
        "Adoption mechanism is pinned in-process by "
        "conformance_file_provider_client.",
    ),
    pytest.mark.tier_3,
]


@pytest.mark.timeout(1200)
def test_a_deposit_is_adopted_as_a_placeholder_by_the_file_provider_refresh(
    consent_nest, consent_bridge, metadata_server, fauna_fp_host_app
):
    nest = consent_nest
    alice = nest["user"]
    fauna = fauna_fp_host_app
    folder_name = "fp-deposit-" + secrets.token_hex(4)
    device_id = secrets.token_bytes(32)
    owner_seed = bytes(alice["signing_key"])
    backup_key = blake3.blake3(owner_seed, derive_key_context=_BACKUP_KEY_CONTEXT).digest()

    # ── Fixture setup: the owner's folder, recipient seal key and a
    # write-capable device (the host's seat). ──
    with alice_client(nest) as ws:
        folder = ws.call("fauna.folders.create", {"name": folder_name})["id"]
        provision_recipient_seal_key(ws, alice["actor_id_bytes"])
        ws.call(
            "fauna.sync.register",
            {"device_id": device_id.hex(), "label": "fp-ext", "capabilities": "read,write"},
        )
    folder_ref = f"local:{folder}"

    # ── Fixture setup: the third party's consent and one deposit over the HTTP door. ──
    client, holder_pub = _client(nest, metadata_server, BARE_SCOPE)
    tokens = _approve_with_grant(nest, client, holder_pub, folder, BARE_SCOPE, choose=True)
    status, body = _http_deposit(nest, client, tokens["access_token"], folder, HTTP_NAME, HTTP_MARKER)
    assert status == 202 and body == {"accepted": True}, (status, body)
    assert len(_inbox(nest, folder)) == 1, "the deposit is parked before the host adopts it"

    before = set(os.listdir(_CLOUD)) if _CLOUD.exists() else set()
    try:
        provisioned = _run_host(
            fauna, "provision", nest["url"], alice["actor_id_bytes"].hex(), device_id.hex(),
            "fp-ext", backup_key.hex(), alice["token"], check=False,
        )
        if provisioned.returncode != 0 and "-34018" in provisioned.stdout + provisioned.stderr:
            # Signing identities exist but the app carries no embedded provisioning
            # profile, so the app-group Keychain refuses the credential write.
            pytest.skip(
                "app-group data-protection Keychain refused `provision` (errSecMissingEntitlement "
                "-34018): the build has no embedded provisioning profile"
                "; the test runs once the profile lands."
            )
        assert provisioned.returncode == 0, (
            f"`Fauna provision` failed: {provisioned.stdout.strip()} {provisioned.stderr.strip()}"
        )
        _run_host(fauna, "register", folder_ref, folder_name)
        _CLOUD.mkdir(parents=True, exist_ok=True)
        domain_dir = _wait_for(
            lambda: next((_CLOUD / d for d in set(os.listdir(_CLOUD)) - before), None),
            timeout=45,
            msg=f"no new domain directory appeared under {_CLOUD} after register",
        )

        # The nudge: signalEnumerator -> enumerateChanges -> host.refresh(), whose
        # adoption pass folds the parked deposit into the replica.
        _run_host(fauna, "signal", folder_ref, folder_name)
        _wait_for(
            lambda: HTTP_NAME in _safe_listdir(domain_dir),
            timeout=60,
            msg=f"{HTTP_NAME} never listed in {domain_dir} after `Fauna signal` — the host's "
            "refresh did not adopt the parked deposit (or the File Provider extension is "
            "DISABLED: System Settings → General → Login Items & Extensions → Extensions).",
        )
    finally:
        _run_host(fauna, "remove", folder_ref, folder_name, check=False)
        _run_host(fauna, "revoke", check=False)

    # Adoption retired the parked item: the owner's inbox is empty.
    def _parked_items() -> list:
        with alice_client(nest) as ws:
            return ws.call("fauna.folders.deposits.list", {"folder_id": folder, "after": 0}).get("items", [])

    _wait_for(
        lambda: not _parked_items(),
        timeout=30,
        msg="adoption left the deposit parked: fauna.folders.deposits.list is not empty",
    )

    for path, raw in _everything_on_disk(nest):
        for marker in (HTTP_MARKER, HTTP_NAME.encode()):
            assert marker not in raw, f"{marker!r} rests in cleartext in {path}"
