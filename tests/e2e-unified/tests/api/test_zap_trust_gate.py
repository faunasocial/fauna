"""A zap counts as a tip only from a signer the payee named, and never on a
post that is not theirs — the witnesses for `docs/features/paid-posts-and-tips.md`
outcomes 19 and 20.

`docs/goal/behavior/monetization.md` § Zap receipts — the trust model: a
kind-9735 receipt's own signature proves nothing, so the nest believes one only
when the payee designated its signer — a payee who has designated nobody
believes nobody — and, when it names a zapped event, only when that event is
one the box holds authored by the payee (conjunct (iii)). The gate runs at
ingest: a refused receipt rests nowhere and counts nothing.

The drive is the production one: the payee links a Nostr key, shares a post onto
the nest's own relay, and designates signers over WS-RPC
(`fauna.nostr.zap_signers.add`); a Lightning provider then posts its signed
receipt to the relay as any Nostr client does. The relay's ``OK`` verdict and
the zapped event's `nostr.zaps.total` are the observables. The in-process twins
are `bins/fauna-nest/tests/conformance_nostr_zap_trust_gate.rs`.

tier_3: a real `fauna-nest` binary over its production doors; no app.
"""

from __future__ import annotations

import os

import pytest

from helpers import nostr_payee, nostr_relay
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from helpers.zap_receipts import ZapSigner

pytestmark = pytest.mark.tier_3

#: `helpers.zap_receipts.LNBC_21_SATS`, counted: ``(total_msats, zap_count)``.
ONE_21_SAT_ZAP = (21_000, 1)
NOTHING = (0, 0)


def _designate(nest, user, signer: ZapSigner) -> None:
    with nostr_payee.ws(nest, user) as ws:
        ws.call(
            "fauna.nostr.zap_signers.add",
            {"signer_pubkey": signer.pubkey, "label": "my wallet provider"},
        )


def _zaps(nest, user, event_id: str) -> tuple[int, int]:
    with nostr_payee.ws(nest, user) as ws:
        reply = ws.call("nostr.zaps.total", {"event_id": event_id})
    return reply["total_msats"], reply["zap_count"]


def _report(nest, signer: ZapSigner, *, payee_pubkey: str, event_id: str) -> tuple[bool, str]:
    """``signer`` reports a 21-sat tip from a fresh tipper; the relay's verdict."""
    receipt = signer.receipt(
        payee_pubkey=payee_pubkey, target_event_id=event_id, tipper_secret=os.urandom(32),
    )
    return nostr_relay.publish(nostr_relay.nest_relay_url(nest), receipt)


def _assert_refused(verdict: tuple[bool, str], why: str) -> None:
    accepted, message = verdict
    assert not accepted and message.startswith("restricted:"), (
        f"{why}; the relay answered accepted={accepted} {message!r}"
    )


def _wait_counted(nest, user, event_id: str, expected: tuple[int, int]) -> None:
    wait_until(
        lambda: _zaps(nest, user, event_id) == expected or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the zap total for {event_id} is {_zaps(nest, user, event_id)}, "
            f"expected {expected}"
        ),
    )


@pytest.mark.feature("paid-posts-and-tips")
def test_with_no_signer_named_no_zap_counts(nest_instance):
    nest = nest_instance
    user, _post_id, note = nostr_payee.shared_post(nest)

    # The out-of-the-box state: the payee has named nobody, so even a perfect
    # receipt for their own shared post is inert.
    _assert_refused(
        _report(nest, ZapSigner(), payee_pubkey=note["pubkey"], event_id=note["id"]),
        "a payee who named nobody believes nobody",
    )
    assert _zaps(nest, user, note["id"]) == NOTHING, "an unbelieved receipt counts nothing"


@pytest.mark.feature("paid-posts-and-tips")
def test_a_zap_counts_only_from_the_signer_the_payee_named(nest_instance):
    nest = nest_instance
    user, _post_id, note = nostr_payee.shared_post(nest)
    named, stranger = ZapSigner(), ZapSigner()
    _designate(nest, user, named)

    # The same receipt shape, from a signer the payee did not name.
    _assert_refused(
        _report(nest, stranger, payee_pubkey=note["pubkey"], event_id=note["id"]),
        "a receipt from a signer the payee did not name is refused",
    )
    assert _zaps(nest, user, note["id"]) == NOTHING, "an unbelieved receipt counts nothing"

    accepted, message = _report(nest, named, payee_pubkey=note["pubkey"], event_id=note["id"])
    assert accepted, f"the named signer's receipt must be believed: {message!r}"
    _wait_counted(nest, user, note["id"], ONE_21_SAT_ZAP)


@pytest.mark.feature("paid-posts-and-tips")
def test_a_named_signer_cannot_credit_a_tip_to_another_users_post(nest_instance):
    nest = nest_instance
    victim, _victim_post, victim_note = nostr_payee.shared_post(nest)
    payee, _own_post, own_note = nostr_payee.shared_post(nest)
    signer = ZapSigner()
    _designate(nest, payee, signer)

    # Every `p`-tag conjunct passes — the receipt is addressed to the payee who
    # named this signer — but its `e` names someone else's note on this box.
    _assert_refused(
        _report(nest, signer, payee_pubkey=own_note["pubkey"], event_id=victim_note["id"]),
        "a receipt crediting another user's post is refused",
    )
    assert _zaps(nest, victim, victim_note["id"]) == NOTHING, (
        "nothing is attributed to the other user's post"
    )

    # The control: the same signer, the same payee, the payee's own post.
    accepted, message = _report(
        nest, signer, payee_pubkey=own_note["pubkey"], event_id=own_note["id"],
    )
    assert accepted, f"the named signer's receipt on the payee's own post: {message!r}"
    _wait_counted(nest, payee, own_note["id"], ONE_21_SAT_ZAP)
    assert _zaps(nest, victim, victim_note["id"]) == NOTHING
