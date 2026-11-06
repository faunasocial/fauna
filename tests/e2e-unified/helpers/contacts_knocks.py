"""The Contacts page's knock list, read the way every knock journey reads it.

Shared by the suites that accept, block or dismiss a knock through the app UI
(``test_contacts_edge_actions.py``, ``test_contacts_accept_opens_conversation.py``):
open the People half, wait for a sender's knock to list, and read a list the
page may re-render under the read.
"""

from __future__ import annotations

from helpers import budgets
from helpers.waiting import wait_until


def open_people(app) -> None:
    """The Contacts page on its People half. The segment is sticky across
    navigation, so an earlier Address Book visit would otherwise leave the
    roster and the knock list unpainted."""
    app.contacts.navigate()
    app.contacts.switch_to_people()


def read_racy(fn):
    """One poll's read of a list the page may re-render under it. A read racing
    a re-render answers ``None`` and the next poll reads the whole list again;
    one that never succeeds times out into the caller's diagnosis."""
    try:
        return fn()
    except Exception:  # noqa: BLE001 — see the docstring
        return None


def knock_row(app, sender_hex: str, handle: str | None = None) -> int:
    """Wait for ``sender_hex``'s knock to list, and return its row index.

    ``handle`` is the sender's handle when the caller knows the roster holds it
    (``knock_index_for`` takes either form)."""
    found = wait_until(
        lambda: (lambda i: (i,) if i is not None else None)(
            read_racy(lambda: app.contacts.knock_index_for(sender_hex, handle))
        ),
        budgets.PUSH_REFRESH_S,
        diagnose=lambda: (
            f"the knock from {sender_hex} never listed; knock-sender texts="
            f"{read_racy(app.contacts.knock_senders)!r}; error={app.error_text()!r}"
        ),
    )
    return found[0]
