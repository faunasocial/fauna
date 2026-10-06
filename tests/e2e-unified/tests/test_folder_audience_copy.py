"""tier_3 — what the audience and website controls SAY (`folder-audience-hint`,
`folder-website-hint`, `folder-audience-public-names-warning`,
`folder-audience-public-reseal-warning`; IDs user-approved 2026-09-25).

`test_folder_audience_control.py` proves what the controls DO — the confirm
arms before it publishes, the website toggle is orthogonal to the audience. This
module proves the three sentences a user reads beside them, each of which exists
because a user was surprised without it (`ui/folders.md` § Audience and website
serving):

- **the website hint is a tri-state on the live serving picture.** Publishing a
  site takes switches in two places — this page's audience + website toggle, and
  the actor's own web-address opt-in on Settings → Web — and a user who flipped
  only the folder half used to be told nothing while the nest served its info
  page in their site's place. So the hint must move through all three states as
  the user walks the journey, and the middle one must name the other switch;
- **the go-public confirmation states both consequences** — names and paths go
  public too, and going private again protects only what is added afterwards —
  and it is answered on the owner-ATTESTED path (`FoldersClient::set_audience`
  under `with_audience_attestor`), so the dialog a user reads is the one whose
  answer a verifying seat will honour;
- **a shared folder offers no way to make it private, and says why.** `private`
  is withheld from the picker while the folder is bound (the nest refuses it),
  and the hint says the sharing has to go first. Until this module the option
  set was pinned only in `fauna-folders-machine`'s unit tests.

Every sentence is asserted against the i18n string it renders (`S.devices.*`),
never a hand-typed literal, so a copy edit regenerates the expectation instead of
turning the assertion into a tautology. MUTATION is UI-driven throughout
(convention 8): the select is picked, the confirm and the toggles clicked, the
share walked through the recipient picker. The wizard and the headless
recipient are fixture setup (the documented carve-out); VERIFICATION of what
moved reads the nest's own row.

tui leads; the other six columns follow as one lift, and
this marker list is the parity ledger.
"""

from __future__ import annotations

import secrets

import pytest

from helpers.waiting import wait_until
from i18n.strings import S
from helpers.set_names import find_set

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

# Generous ceilings, not expectations (convention 14): every wait below is a
# deadline poll on STATE, never a settle-sleep.
_FLAG_S = 60.0
_UI_S = 30.0
_ROSTER_S = 30.0


def _folder_row(nest_url: str, actor, name: str) -> dict | None:
    """The actor's own ``fauna.folders.list`` row for ``name`` — the nest's
    ground truth, never the app's rendering of it."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(
        nest_url,
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    ) as c:
        reply = c.call("fauna.folders.list", {})
    return find_set(reply.get("folders", []), name)


def _await_row(nest_url: str, actor, name: str, **want) -> dict:
    def _matches():
        row = _folder_row(nest_url, actor, name)
        if row is None:
            return None
        for key, value in want.items():
            got = row.get(key)
            if (bool(got) if isinstance(value, bool) else got) != value:
                return None
        return row

    return wait_until(
        _matches,
        _FLAG_S,
        interval=0.5,
        diagnose=lambda: (
            f"wanted {want}, nest row is {_folder_row(nest_url, actor, name)!r}"
        ),
    )


def _open_row(b, name: str) -> None:
    """Expand ``name``'s row unless its body already shows.
    ``find_and_expand_folder`` TOGGLES, so an unconditional call on an open row
    would close the very body the next read addresses."""
    if not b.audience_select_visible():
        b.find_and_expand_folder_until(name, "folder-audience-select")


def _await_website_hint(app, name: str, want: str, why: str) -> None:
    b = app.backups

    def _reads():
        _open_row(b, name)
        return b.website_hint_text() == want

    wait_until(
        _reads,
        _UI_S,
        diagnose=lambda: (
            f"{why}: folder-website-hint should read {want!r}, reads "
            f"{b.website_hint_text()!r}; error={app.error_text()!r}"
        ),
    )


@pytest.mark.parametrize("folder_share_owner_app", ["tui"], indirect=True)
@pytest.mark.real_conversations
@pytest.mark.feature("public-folders-and-websites")
def test_a_website_folder_says_whether_anyone_can_reach_it(folder_share_owner_app):
    """Outcome 5: a folder set to serve a website says whether anyone can really
    reach it — and while they cannot, names the switch still to turn on.

    Walked in the order a user publishes a site, so each state is the one the
    previous gesture left: website on but private → the audience wording; made
    public with the web address still off (its default) → the nobody-can-reach-it
    wording naming Settings → Web; the address switched on there → served."""
    app, nest, owner = folder_share_owner_app
    b = app.backups
    name = f"reach-{secrets.token_hex(4)}"

    b.navigate_folders()
    b.create_folder_via_wizard(name)
    _open_row(b, name)
    b.toggle_website()
    _await_row(nest["url"], owner, name, website_enabled=True)

    # ── 1. Published, but nobody may read it yet. ──
    _await_website_hint(
        app,
        name,
        S.devices.serve_website_needs_audience,
        "a website folder that is still private",
    )

    # ── 2. Public — but the web address is off, so nobody can reach it. ──
    b.make_public(name)
    _await_row(nest["url"], owner, name, audience="public")
    _await_website_hint(
        app,
        name,
        S.devices.serve_website_address_off,
        "a public website folder while the owner's web address is off (the "
        "default) — the one misleading case, which must name the other switch",
    )

    # ── 3. The other switch, turned on where the hint said it lives. ──
    app.web_settings.navigate()
    app.web_settings.set_subdomain_enabled(True)
    b.navigate_folders()
    _await_website_hint(
        app,
        name,
        S.devices.serve_website_live,
        "a public website folder with the web address on",
    )


@pytest.mark.parametrize("folder_share_owner_app", ["tui"], indirect=True)
@pytest.mark.real_conversations
@pytest.mark.feature("public-folders-and-websites")
def test_the_go_public_confirmation_states_both_consequences(folder_share_owner_app):
    """Outcome 7: before a folder goes public, the confirmation says that its
    file and folder names become public too, and that making it private again
    protects only what is added afterwards.

    Both lines are present exactly while the confirm is, and the answer lands on
    the owner-attested path: the flip carries the owner's attestation, which is
    what a verifying seat unseals on (`with_audience_attestor`)."""
    app, nest, owner = folder_share_owner_app
    b = app.backups
    name = f"declassify-{secrets.token_hex(4)}"

    b.navigate_folders()
    b.create_folder_via_wizard(name)
    _open_row(b, name)
    assert app.driver.is_absent("folder-audience-public-names-warning"), (
        "the consequence lines belong to the armed confirm, not the resting row"
    )

    b.set_audience("public")
    wait_until(
        b.declassify_confirm_visible,
        _UI_S,
        diagnose=lambda: f"confirm not armed; error={app.error_text()!r}",
    )
    names, reseal = b.declassify_warnings()
    assert names == S.devices.declassify_body, (
        f"the confirmation must say names and paths go public too; got {names!r}"
    )
    assert reseal == S.devices.declassify_irreversible, (
        "the confirmation must say going private again protects only what is "
        f"added afterwards; got {reseal!r}"
    )

    b.confirm_public()
    row = _await_row(nest["url"], owner, name, audience="public")
    attestation = row.get("audience_attestation")
    assert attestation is not None and bytes(attestation["owner"]) == owner[
        "actor_id_bytes"
    ], (
        "the confirm the user just read must land OWNER-ATTESTED — an unattested "
        "flip is one every verifying seat keeps sealed, so the dialog would be "
        f"promising a publication that never happens. Nest row: {row!r}"
    )
    wait_until(
        lambda: app.driver.is_absent("folder-audience-public-reseal-warning"),
        _UI_S,
        diagnose=lambda: "the consequence lines outlived the answered confirm",
    )


@pytest.mark.parametrize("folder_share_owner_app", ["tui"], indirect=True)
@pytest.mark.real_conversations
@pytest.mark.feature("public-folders-and-websites")
def test_a_shared_folder_offers_no_private_and_says_why(folder_share_owner_app):
    """Outcome 8: a folder you are sharing offers no way to make it private while
    it is still shared, and says the sharing has to go first.

    The folder is bound the one way a folder becomes bound — the owner's share
    gesture — and the picker is then read whole: `private` must be absent (the
    nest refuses it while bound), and the hint must explain the repair."""
    from common.auth import register_handled_actor
    from conftest import MAIL_PRIMARY_DOMAIN
    from tests.api import conv_api

    app, nest, owner = folder_share_owner_app
    b = app.backups

    # A headless recipient with a fetchable KeyPackage — bound-ness is minted by
    # the owner's gesture and waits on nobody accepting.
    recipient_handle = "shbob" + secrets.token_hex(2)
    recipient = register_handled_actor(
        nest["port"], handle=recipient_handle, domain=MAIL_PRIMARY_DOMAIN
    )
    conv_api.keypackage_upload(
        nest["port"],
        recipient,
        conv_api.mint_key_packages(bytes(recipient["signing_key"]), 2),
    )

    name = f"shared-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    b.find_and_expand_folder(name)

    # The unbound baseline, so the absence below is a change and not a picker
    # that never offered `private` at all.
    assert "private" in (b.audience_options() or []), (
        f"an unbound folder offers private; got {b.audience_options()!r}"
    )

    b.open_share_dialog()
    assert b.share_dialog_open(), (
        "folder-share-button must open the recipient picker: "
        f"{app.driver.diagnose('recipient-picker-input')}"
    )
    b.share_recipient(handle=recipient_handle, actor_id_hex=recipient["actor_id_hex"])
    wait_until(
        lambda: b.shared_member_count() == 1,
        _ROSTER_S,
        diagnose=lambda: (
            "the share never landed a roster member, so the folder never bound. "
            f"nest roster={conv_api.folder_member_actors(nest['port'], owner, name)!r} "
            f"error={app.error_text()!r}"
        ),
    )

    def _bound_picture():
        _open_row(b, name)
        return b.audience_current_value() == "shared"

    wait_until(
        _bound_picture,
        _UI_S,
        diagnose=lambda: (
            f"a shared folder paints `shared`; got {b.audience_current_value()!r}"
        ),
    )
    options = b.audience_options()
    assert options is not None and "private" not in options, (
        "a shared folder must offer no way to make it private while it is "
        f"shared — the nest refuses it. Options offered: {options!r}"
    )
    assert b.audience_hint_text() == S.devices.folder_audience_shared_hint, (
        "the hint must say the sharing has to go first; got "
        f"{b.audience_hint_text()!r}"
    )
