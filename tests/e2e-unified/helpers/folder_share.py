"""The owner-side folder share choreography shared by the cross-user journeys:
a person an owner can share with who never opens an app, the share driven
through the owner's own controls, and the two reads a removal is judged by
(the owner's roster by identity, the nest's key-bundle answer to a member).

Lifted out of `tests/test_folder_member_media_decrypt.py` when the interrupted
member removal journey (`tests/test_crash_recovery_journeys.py`) needed the
same four steps — one home, so the two cannot drift.
"""
from __future__ import annotations

import secrets

from helpers.set_names import addressed
from helpers.waiting import wait_until

#: The owner's roster reflecting a share or a removal. A named generous budget
#: polled to a deadline — a green run pays only what it uses (convention 14).
SHARE_ROSTER_S = 20.0


def roster_index(ob, handle: str, row: int | None = None) -> int:
    """The owner-side roster index of the member whose handle is ``handle`` —
    by identity, never by position (a guessed index would act on the wrong
    person).

    A member on another nest is labelled ``handle@<their nest's domain>`` (the
    name their own nest announced and the owner's nest verified —
    ``federation-shared-folders.md`` § Cross-nest shared folders + channel
    append, the id→handle bullet → *The folder roster*), so pass the qualified
    form to match them exactly; the bare handle matches it too."""
    handles = [ob.member_handle(i, row=row) for i in range(ob.shared_member_count())]
    index = next((i for i, h in enumerate(handles) if handle in h), None)
    assert index is not None, f"{handle!r} is not on the owner's roster: {handles!r}"
    return index


def content_key_get(nest, actor, set_name: str):
    """``fauna.folders.content_key.get`` as ``actor`` — the call a member makes
    to fetch the sealed key bundle that opens the set's content. Returns the
    reply, or the refusal as a string."""
    from common.auth import _user_call

    try:
        return _user_call(
            nest["port"],
            actor["signing_key"].encode().hex(),
            "fauna.folders.content_key.get",
            addressed(set_name),
            nest["url"],
        )
    except Exception as exc:  # the refusal IS the observable
        return f"refused: {exc}"


def headless_member(nest, prefix: str) -> dict:
    """A handled actor with real published KeyPackages, so an owner's share can
    admit them — a person on the roster who never opens an app."""
    from conftest import MAIL_PRIMARY_DOMAIN
    from helpers.admin_wire import admit_adult
    from tests.api import conv_api

    actor = admit_adult(
        nest, prefix + secrets.token_hex(3), domain=MAIL_PRIMARY_DOMAIN, token=True
    )
    stored = conv_api.keypackage_upload(
        nest["port"], actor, conv_api.mint_key_packages(actor["signing_key"].encode(), 2)
    )
    assert stored >= 1, f"{prefix} must publish a fetchable KeyPackage; stored={stored}"
    return actor


def channel_id_for_group_hex(group_id_hex: str) -> str:
    """The set's share-plane ``ChannelId`` (hex) for its raw MLS group id —
    ``fauna_core::folder_keys::channel_id_for_group``, BLAKE3 ``derive_key``
    under the ``fauna.channel.v1`` context. The identity a cross-nest member's
    custody and Media filter address the set by (``foreign:<channel>``)."""
    import blake3

    return blake3.blake3(
        bytes.fromhex(group_id_hex), derive_key_context="fauna.channel.v1"
    ).hexdigest()


def share_through_owner_ui(owner_app, set_name: str, person: dict, want: int) -> int:
    """Share ``set_name`` with ``person`` from the owner's own controls and wait
    for the roster to reach ``want``. Returns the owner's folder-row index."""
    ob = owner_app.backups
    ob.navigate_folders()
    # `find_and_expand_folder` TOGGLES, so re-open a row the previous step left
    # open.
    row = ob.find_and_expand_folder(set_name)
    if not ob.share_button_visible():
        ob.expand_folder(row)
    ob.open_share_dialog()
    ob.share_recipient(handle=person["handle"], actor_id_hex=person["actor_id_hex"])
    wait_until(
        lambda: ob.shared_member_count() == want,
        SHARE_ROSTER_S,
        diagnose=lambda: (
            f"sharing with {person['handle']!r} should bring the roster to {want}; it "
            f"reads {ob.shared_member_count()} error={owner_app.error_text()!r}"
        ),
    )
    return row


def share_cross_nest_through_owner_ui(
    owner_app, set_name: str, qualified: str, *, role: str | None = None,
    want: int = 1, budget_s: float | None = None,
) -> int:
    """Share ``set_name`` from the owner's own controls with ``qualified`` — a
    ``handle@<nest authority>`` on ANOTHER nest, typed verbatim (the shape only
    an app resolving through the shared ``find_user`` can share with) — granting
    ``role`` (``'reader'``/``'writer'`` on ``folder-share-role-select``; ``None``
    leaves the dialog's default, Reader), and wait for the roster to reach
    ``want``. Returns the owner's folder-row index.

    The cross-nest twin of :func:`share_through_owner_ui`: the roster wait is a
    cross-nest hop (the home nest fetches the member's KeyPackage over the
    relay), so its budget is ``budgets.CROSS_NEST_S``."""
    from helpers import budgets

    ob = owner_app.backups
    ob.navigate_folders()
    row = ob.find_and_expand_folder(set_name)
    if not ob.share_button_visible():
        ob.expand_folder(row)
    ob.open_share_dialog()
    if role is not None:
        ob.pick_share_role(role)
    ob.share_recipient_typed(qualified)
    wait_until(
        lambda: ob.shared_member_count() == want or None,
        budget_s if budget_s is not None else budgets.CROSS_NEST_S,
        diagnose=lambda: (
            f"sharing {set_name!r} with {qualified!r} (role={role or 'default'}) never "
            f"brought the owner's roster to {want}; count={ob.shared_member_count()} "
            f"error={owner_app.error_text()!r}"
        ),
    )
    return row
