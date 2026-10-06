"""How an automation agent says "there is nothing here to activate".

A click or key press on an element that carries no gesture — a reader's
body-less folder row, a mail chip whose text already is the address, the
critical-alert row — is refused by some agents and accepted as a no-op by
others. Each refusing agent words it its own way, and a journey that tolerates
the refusal must tolerate every app's wording: a test that matched one app's
phrase passed there and raised on the next app (`test_folder_writer_revocation`
swallowed tui's and apple's "not actuable" and failed on linux's "not
activatable").

One classification, so the journeys cannot drift apart again. Any other
failure of the gesture is a real one and must still raise.
"""
from __future__ import annotations

#: The refusal each agent answers, by the phrase its error carries:
#: tui and apple "not actuable"; linux "not activatable" (a click) — and, for a
#: key an agent has no arm for, tui's and linux's "is not driven on".
_INERT_PHRASES = ("not actuable", "not activatable", "is not driven on")


def is_inert_refusal(exc: BaseException) -> bool:
    """Whether `exc` is an agent refusing a gesture on an inert element."""
    text = str(exc)
    return any(phrase in text for phrase in _INERT_PHRASES)
