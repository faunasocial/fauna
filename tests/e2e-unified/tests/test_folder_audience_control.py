"""tier_3 — the audience + website controls themselves (`folder-audience-select`,
`folder-audience-public-confirm`, `folder-website-toggle`; folders re-model
phase 4 slice 4d, tui-led 2026-08-19).

**Why this exists beside `test_public_website_folder_serve.py`.** That test
proves the whole journey — public folder → served bytes at the owner's address —
and needs a sync agent and a bound location to do it, which makes it a
desktop-shaped test that web can never join. The *controls* are cross-app, so
their contract needs a cross-app home. This is it: no agent, no file, no serving,
just the three things the control layer promises, verified against the nest's own
row.

The three, and why each is a separate assertion rather than one end-state check:

1. **Picking `public` arms; it does not publish.** The declassify confirm is the
   gate on the one audience that rests UNSEALED — content and names/paths alike
   (`principles.md` § The user always controls their data owns that exception).
   A regression that published on the bare select would pass any test that only
   looked at the end state, so the un-answered state is asserted on the NEST:
   nothing may have moved yet.
2. **While armed, the select keeps painting the folder's CURRENT audience.**
   Showing `public` before the answer would report an audience the folder does
   not have — the same non-optimistic rule the WebDAV toggle follows. This is the
   assertion no other test makes, and it is the one an app is most likely to get
   wrong by binding the select straight to local state.
3. **The website toggle is orthogonal to the audience**, and flipping it moves
   `website_enabled` on the nest row.

MUTATION is UI-driven throughout (convention 8 — the select is selected and the
confirm is clicked, never a raw `folders.update`); the folder is fixture setup
via the ordinary wizard (the documented carve-out), and VERIFICATION reads the
nest's ground truth through `fauna.folders.list`, the black-box idiom its
siblings `test_folder_place_editor.py` / `test_folder_nest_place.py` use.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.audience_attestation import attestation_message
from helpers.set_names import find_set
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    # tui led slice 4d; web joins with its leg, and
    # linux with the same row's Linux-desktop leg. The other four join as theirs
    # land — this marker list is the parity ledger, exactly as
    # `test_folder_place_editor.py` keeps its own.
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    # macos + ios, 2026-08-28: the fourth and fifth apps, joining together
    # because both render the ONE shared FaunaKit `FolderAudienceSelect` /
    # `FolderWebsiteToggle`. Their picker needs no by-hand snap-back — the
    # SwiftUI `Picker` binding reads the live snapshot, so assertion 2 holds
    # structurally rather than by correction, which is the opposite of what web
    # and linux each measured. That is precisely why they join THIS test: it is
    # the one check that can tell a structurally-correct paint from a lucky one.
    pytest.mark.macos,
    pytest.mark.ios,
    # windows, 2026-08-29: the sixth app. Its `ComboBox` commits a pick
    # immediately — the GTK/DOM shape, not apple's — so its leg carries the
    # by-hand snap-back, the same correction `folder-nest-residency-select`
    # beside it already needed. Assertion 2 is what tells the two apart, which is
    # the whole reason this test is where a leg reports.
    pytest.mark.windows,
]

# Generous ceilings, not expectations (convention 14): every wait below is a
# deadline poll on STATE, never a settle-sleep.
_FLAG_WINDOW_SECS = 30.0
_UI_WINDOW_SECS = 15.0


def _user_client(nest_instance, test_user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )


def _folder_row(client, name: str) -> dict | None:
    """The nest's own row for `name`, or None. Ground truth — never the app's
    rendering of it."""
    with client:
        reply = client.call("fauna.folders.list", {})
    return find_set(reply.get("folders", []), name)


def _await_folder_flags(
    client,
    name: str,
    *,
    audience: str | None = None,
    website_enabled: bool | None = None,
) -> dict:
    """Poll the nest row until the named flags hold.

    The failure names both the wanted and the observed row: the two interesting
    ways this fails — the gesture never reached the nest, and the nest refused
    the transition — are distinguishable only from the row itself.
    """

    def _matches():
        row = _folder_row(client, name)
        if row is None:
            return None
        if audience is not None and row.get("audience") != audience:
            return None
        if (
            website_enabled is not None
            and bool(row.get("website_enabled")) is not website_enabled
        ):
            return None
        return row

    want = {"audience": audience, "website_enabled": website_enabled}
    return wait_until(
        _matches,
        _FLAG_WINDOW_SECS,
        interval=0.5,
        diagnose=lambda: (
            f"wanted {want}, nest row is {_folder_row(client, name)!r}"
        ),
    )


@pytest.mark.feature("public-folders-and-websites")
def test_the_audience_control_arms_before_it_publishes(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    b = app.backups
    b.navigate_folders()

    name = f"audience-{secrets.token_hex(4)}"
    b.create_folder_via_wizard(name)
    client = _user_client(nest_instance, test_user)

    # The control renders on expand, and reads `private` — the fail-closed
    # normalization, which is also what a fresh folder genuinely is.
    b.find_and_expand_folder(name)
    app.driver.wait_for("folder-audience-select", timeout=_UI_WINDOW_SECS)
    assert b.audience_current_value() == "private", (
        "a fresh folder paints `private`; anything else means the select is "
        "showing a raw column value instead of the normalized one"
    )

    # ── 1. Picking `public` ARMS. It must not publish. ──
    b.set_audience("public")
    wait_until(
        b.declassify_confirm_visible,
        _UI_WINDOW_SECS,
        diagnose=lambda: (
            f"confirm not armed; error={app.error_text()!r}"
        ),
    )

    # ── 2. …and while armed, the select still paints the CURRENT audience. ──
    assert b.audience_current_value() == "private", (
        "while the confirm is armed the select must keep painting the folder's "
        "CURRENT audience — painting `public` before the answer reports an "
        "audience the folder does not have"
    )

    # …and the nest has not moved. This is the assertion that makes the arming
    # real rather than decorative.
    row = _folder_row(client, name)
    assert row is not None and row.get("audience") != "public", (
        "arming the declassify confirm must write NOTHING — a folder that went "
        "public on the bare select would rest unsealed, names and paths "
        "included, without its owner having answered the one gate that exists "
        f"to ask them. Nest row: {row!r}"
    )

    # ── 3. Answering the confirm is what flips it. ──
    b.confirm_public()
    _await_folder_flags(client, name, audience="public")


def test_the_declassify_confirm_carries_the_owners_attestation(
    logged_in_app, nest_instance, test_user
):
    """The answered `folder-audience-public-confirm` is the attestation's origin
    (`encryption-at-rest.md` § Readable classes → *The declassification is
    owner-ATTESTED*): the flip the app lands carries an Ed25519 statement by the
    OWNER's identity key over this folder's id, its name and a counter, which the
    nest stores opaquely and serves back on the row. A seat unseals only on that
    signature, never on the nest's bare `audience` — so a flip that lands
    unattested is one every verifying seat keeps sealed. This is the writer half's
    witness on every app: the ground truth is the nest's row, verified here with
    the owner's public key the way a seat verifies it.
    """
    app = logged_in_app
    b = app.backups
    b.navigate_folders()

    name = f"attest-{secrets.token_hex(4)}"
    b.create_folder_via_wizard(name)
    client = _user_client(nest_instance, test_user)

    b.find_and_expand_folder(name)
    app.driver.wait_for("folder-audience-select", timeout=_UI_WINDOW_SECS)
    b.set_audience("public")
    wait_until(
        b.declassify_confirm_visible,
        _UI_WINDOW_SECS,
        diagnose=lambda: f"confirm not armed; error={app.error_text()!r}",
    )
    b.confirm_public()
    row = _await_folder_flags(client, name, audience="public")

    attestation = row.get("audience_attestation")
    assert attestation is not None, (
        "the flip landed with NO attestation — the app's `set_audience` ran "
        "without the owner's identity key wired (`with_audience_attestor`), so "
        f"every verifying seat keeps this folder sealed. Nest row: {row!r}"
    )
    owner = bytes(test_user["signing_key"].verify_key)
    assert bytes(attestation["owner"]) == owner, (
        f"attested by {bytes(attestation['owner']).hex()}, not the owner {owner.hex()}"
    )
    assert attestation["folder_id"] == row["id"], (
        f"attests folder {attestation['folder_id']}, the row is {row['id']}"
    )
    from nacl.exceptions import BadSignatureError
    from nacl.signing import VerifyKey

    message = attestation_message(owner, row["id"], name, attestation["counter"])
    try:
        VerifyKey(owner).verify(message, bytes(attestation["sig"]))
    except BadSignatureError:
        pytest.fail(
            "the served attestation does not verify under the owner's key over "
            f"(id={row['id']}, name={name!r}, counter={attestation['counter']}) — "
            "a seat would seal this folder"
        )


@pytest.mark.feature("public-folders-and-websites")
def test_the_website_toggle_is_orthogonal_to_the_audience(
    logged_in_app, nest_instance, test_user
):
    """The toggle publishes the folder's HEAD; the audience decides who may read
    it. So it is offered — and works — on a folder that is not public, where it
    is real but inert. Disabling it there would strand a user with no way to
    prepare a site before publishing it."""
    app = logged_in_app
    b = app.backups
    b.navigate_folders()

    name = f"website-{secrets.token_hex(4)}"
    b.create_folder_via_wizard(name)
    client = _user_client(nest_instance, test_user)

    b.find_and_expand_folder(name)
    app.driver.wait_for("folder-website-toggle", timeout=_UI_WINDOW_SECS)

    # Still `private` — the point of this test is that the toggle does not need
    # a readable audience to be usable.
    assert b.audience_current_value() == "private"
    assert b.website_toggle_enabled(), (
        "the website toggle stays ENABLED on a folder nobody can read yet — the "
        "setting is real, merely inert, and the app says so with a hint rather "
        "than by disabling the control"
    )

    b.toggle_website()
    _await_folder_flags(client, name, website_enabled=True)

    # And the audience genuinely did not move with it — orthogonal in both
    # directions, not just in the one the happy path exercises.
    row = _folder_row(client, name)
    assert row.get("audience") != "public", (
        f"serving the head must not publish the folder's contents: {row!r}"
    )
