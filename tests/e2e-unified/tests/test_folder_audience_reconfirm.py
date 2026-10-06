"""tier_3 — the owner's re-confirm door for a public folder this seat cannot
verify (`folder-audience-unattested` + `folder-audience-reconfirm-button`;
`ui/folders.md` § Audience and website serving, mechanism
`encryption-at-rest.md` § Readable classes → *The declassification is
owner-ATTESTED*).

A folder the nest reports `public` with no attestation that verifies under the
owner's own key — inherited through a succession or written by a raw
`fauna.folders.update` (the nest stores shape-checked only) — is one every seat keeps
sealed (its served website goes dark) until the owner confirms `public` again.
A select already at `public` fires no change, so the page offers a status line
and a button that ARMS the same declassify confirm; answering it re-sends
`public`, which re-mints.

**Seed:** the unattested state is written the way it arises — a raw
`fauna.folders.update` carrying `audience: public` and no attestation (the nest
stores what it is given, shape-checked only). That is fixture SETUP, standing in
for a succession-inherited folder, never for the user's gesture: the MUTATION under test — press
the button, answer the confirm — is UI-driven (convention 8), and VERIFICATION
reads the nest's own row and checks the signature the way a seat does.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.audience_attestation import attestation_verifies
from helpers.waiting import wait_until
from helpers.set_names import find_set

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    # tui leads (the slice); the other six join with their trickle-down
    # legs — this marker list is the parity ledger.
    pytest.mark.tui,
]

# Generous ceilings, not expectations (convention 14): every wait below is a
# deadline poll on STATE, never a settle-sleep.
_NEST_WINDOW_SECS = 30.0
_UI_WINDOW_SECS = 15.0


def _user_client(nest_instance, test_user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )


def _folder_row(client, name: str) -> dict | None:
    with client:
        reply = client.call("fauna.folders.list", {})
    return find_set(reply.get("folders", []), name)


@pytest.mark.feature("public-folders-and-websites")
def test_an_unattested_public_folder_is_healed_by_the_owners_reconfirm(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    b = app.backups
    client = _user_client(nest_instance, test_user)
    owner = bytes(test_user["signing_key"].verify_key)

    name = f"unattested-{secrets.token_hex(4)}"
    with client:
        client.call(
            "fauna.folders.create",
            {"name": name},
        )
        client.call("fauna.folders.update", {"name": name, "audience": "public"})
    row = _folder_row(client, name)
    assert row is not None and row.get("audience") == "public", row
    assert not attestation_verifies(row, owner, name), (
        f"the seed must be the UNATTESTED public state: {row!r}"
    )

    # Navigating to the page is what hydrates the folder list.
    b.navigate_folders()
    b.find_and_expand_folder(name)
    wait_until(
        b.audience_unattested_visible,
        _UI_WINDOW_SECS,
        diagnose=lambda: (
            "the expanded owner row of an unattested public folder paints no "
            f"folder-audience-unattested; error={app.error_text()!r}"
        ),
    )

    # The button ARMS — it must not write on its own.
    b.reconfirm_public()
    wait_until(
        b.declassify_confirm_visible,
        _UI_WINDOW_SECS,
        diagnose=lambda: f"confirm not armed; error={app.error_text()!r}",
    )
    row = _folder_row(client, name)
    assert not attestation_verifies(row, owner, name), (
        "pressing the re-confirm button must only arm the declassify confirm — "
        f"an attestation landed before the owner answered it: {row!r}"
    )

    # Answering re-sends `public`, which re-mints.
    b.confirm_public()
    healed = wait_until(
        lambda: (
            r
            if (r := _folder_row(client, name)) is not None
            and attestation_verifies(r, owner, name)
            else None
        ),
        _NEST_WINDOW_SECS,
        interval=0.5,
        diagnose=lambda: (
            "no attestation verifying under the owner's key landed after the "
            f"re-confirm; nest row {_folder_row(client, name)!r}, "
            f"error={app.error_text()!r}"
        ),
    )
    assert healed.get("audience") == "public", healed

    # …and the status is gone once the page re-reads the healed row.
    wait_until(
        lambda: not b.audience_unattested_visible(),
        _UI_WINDOW_SECS,
        diagnose=lambda: (
            "folder-audience-unattested still paints on a folder whose served "
            f"attestation now verifies; error={app.error_text()!r}"
        ),
    )
