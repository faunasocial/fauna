"""Nostr — link a key you already have by pasting it.

`docs/goal/ui/nostr.md` § Layout & flow item 1: the link form offers generate,
import and (web only) the browser-extension mode; the import mode takes the
user's own nsec (`nostr-nsec-input`). The outcome is the catalog's
`nostr` 17 — "you can link a Nostr key you already have by pasting it,
instead of having one made for you".

The witness is strong on purpose: "the page now reads linked" would pass just
as well for an app that dropped the pasted secret and generated a fresh key.
So the keypair is minted in the harness (`helpers/nostr_keys.py`, standard
library only), and after the UI link the nest's own `fauna.bridges.list` must
report exactly that public key, in the `imported` signing mode.

tier_3: a real `fauna-nest` binary; the link rides `fauna.bridges.link`
(`bridge_id:"nostr"`, `mode:"import"`). tui first, the lead app; the other
apps implement the same import form and gain their markers in the cross-app
lift.
"""
import pytest

from actions.api_actor import ApiActor
from helpers.nostr_keys import new_keypair

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.android,
]


def _nostr_status(api: ApiActor) -> dict | None:
    return next((b for b in api.bridges_list() if b.get("id") == "nostr"), None)


@pytest.mark.feature("nostr")
def test_a_pasted_nsec_links_exactly_that_key(logged_in_app, nest_instance, test_user):
    """Import mode → paste a harness-minted nsec → link → the nest reports the
    npub of THAT secret and the `imported` mode; unlink restores the form."""
    app = logged_in_app
    api = ApiActor(
        nest_instance["url"], test_user["token"], test_user["actor_id_hex"],
        bytes(test_user["signing_key"]),
    )
    nsec, npub, pubkey_hex = new_keypair()

    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"nostr page not reachable. error: {app.error_text()!r}"
    )
    # The session nest is shared: a prior test may have linked a generated key.
    if app.nostr.is_linked():
        app.nostr.unlink()
        assert app.nostr.wait_for_unlinked(), (
            f"unlink should return to the link form. "
            f"error: {app.nostr.page_error_text()!r}"
        )

    app.nostr.link_import(nsec)
    assert app.nostr.wait_for_linked(), (
        f"pasting an nsec should link the account. "
        f"error: {app.nostr.page_error_text()!r}"
    )
    try:
        status = _nostr_status(api)
        assert status is not None and status.get("linked") is True, (
            f"the nest should report the nostr bridge linked: {status!r}"
        )
        identity = status.get("identity") or {}
        assert identity.get("value") in (npub, pubkey_hex), (
            f"the linked key must be the one pasted (npub {npub}), not a key "
            f"made for the user; the nest reports {identity!r}"
        )
        assert status.get("mode") == "imported", (
            f"a pasted key links in the `imported` signing mode, got "
            f"{status.get('mode')!r}"
        )
    finally:
        app.nostr.unlink()
        assert app.nostr.wait_for_unlinked(), (
            f"unlink should return to the link form. "
            f"error: {app.nostr.page_error_text()!r}"
        )
