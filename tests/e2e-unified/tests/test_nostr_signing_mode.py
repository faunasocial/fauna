"""Nostr — the linked account says how it signs, in plain words.

`docs/goal/ui/nostr.md` § User actions → *Signing-mode display*: a linked
account's signing mode renders as a localized label from the one shared
`fauna_client_bridges::nostr_key_source_label` map, never the raw stored value.
The outcome is the catalog's `nostr` 20.

The witness links twice through the app UI — a key made for the user, then a
harness-minted key pasted in (`helpers/nostr_keys.py`) — and reads
`nostr-signing-mode` after each: the label must be the one the shared map gives
the mode the nest itself reports (`fauna.bridges.list`), so an app that painted
the raw value, or a fixed string, fails one of the two.

tier_3: a real `fauna-nest` binary. tui first, the lead app; the id was promoted
from windows' platform elements to the shared page 2026-10-16. web and linux
carry their markers; android paints the id (compile-verified) and gains its
marker with its first witness run, the other two apps in their own lifts.
"""
import pytest

from actions.api_actor import ApiActor
from helpers.nostr_keys import new_keypair
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.linux, pytest.mark.web]

_LABELS = {
    "generated": S.nostr.account.mode_generated,
    "imported": S.nostr.account.mode_imported,
}


def _nostr_mode(api: ApiActor) -> str | None:
    status = next((b for b in api.bridges_list() if b.get("id") == "nostr"), None)
    return (status or {}).get("mode")


@pytest.mark.feature("nostr")
def test_the_signing_mode_reads_as_its_label(logged_in_app, nest_instance, test_user):
    """Generate → `nostr-signing-mode` reads the generated label; unlink, paste
    an nsec → it reads the imported label; each matches the nest's mode."""
    app = logged_in_app
    api = ApiActor(
        nest_instance["url"], test_user["token"], test_user["actor_id_hex"],
        bytes(test_user["signing_key"]),
    )
    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"nostr page not reachable. error: {app.error_text()!r}"
    )
    # The session nest is shared: a prior test may have linked a key.
    if app.nostr.is_linked():
        app.nostr.unlink()
        assert app.nostr.wait_for_unlinked(), (
            f"unlink should return to the link form. "
            f"error: {app.nostr.page_error_text()!r}"
        )

    nsec, _npub, _hex = new_keypair()
    for want, link in (
        ("generated", app.nostr.link_generate),
        ("imported", lambda: app.nostr.link_import(nsec)),
    ):
        link()
        assert app.nostr.wait_for_linked(), (
            f"the {want} link should link the account. "
            f"error: {app.nostr.page_error_text()!r}"
        )
        try:
            assert _nostr_mode(api) == want, (
                f"the nest should report the {want!r} mode, got {_nostr_mode(api)!r}"
            )
            shown = app.nostr.signing_mode_text()
            assert shown == _LABELS[want], (
                f"nostr-signing-mode should read the shared label for {want!r} "
                f"({_LABELS[want]!r}), got {shown!r}: "
                f"{app.driver.diagnose('nostr-signing-mode')}"
            )
        finally:
            app.nostr.unlink()
            assert app.nostr.wait_for_unlinked(), (
                f"unlink should return to the link form. "
                f"error: {app.nostr.page_error_text()!r}"
            )
        assert app.driver.count("nostr-signing-mode") == 0, (
            "an unlinked account shows no signing mode"
        )
