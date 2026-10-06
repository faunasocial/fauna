"""tier_3: no one but its owner — not another user, not the admin — can read or
train a user's spam filter (`docs/goal/behavior/mail-spam.md` § Cross-actor
isolation), witnessed over the real WS-RPC wire.

The per-user model surface is `fauna.bridges.{fetch,put}_spam_model` (User and
Admin callers are caller-scoped: naming another actor is refused, never silently
redirected — and the nest trains nothing itself, so there is no training surface
beyond the sealed write), and the caller-scoped history/reset kinds that carry
no target field. Each probe is made both as a second user and as the admin; the
victim's stored model bytes and training history are read from the nest's own
table before and after, so "nothing changed" is a settled at-rest fact, not an
absence of an error. Synchronous RPCs, settled reads — convention 14.

The victim is a mail-enabled user (`_provision_msek_recipient`): only such a user
has a recipient key to seal a model and its history to, so only such a user has
a spam filter to protect.
"""

import sqlite3

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register
from conftest import _provision_msek_recipient, _seed_spam_model, _seed_spam_training_history

pytestmark = pytest.mark.tier_3


def _user_ws(nest_instance, user):
    return WsRpcAdminClient(nest_instance["url"], actor_id=user["actor_id_bytes"],
                            signing_key=bytes(user["signing_key"]))


def _admin_ws(nest_instance):
    sk = nest_instance["admin"]["signing_key"]
    return WsRpcAdminClient(nest_instance["url"], actor_id=bytes(sk.verify_key),
                            signing_key=bytes(sk))


def _victim_state(db_path, actor_id):
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        model = conn.execute(
            "SELECT model_json FROM spam_models WHERE actor_id = ?", (actor_id,)
        ).fetchone()
        history = conn.execute(
            "SELECT COUNT(*) FROM spam_training_history WHERE actor_id = ?", (actor_id,)
        ).fetchone()[0]
        return (model[0] if model else None), history
    finally:
        conn.close()


def _refused(call, what):
    with pytest.raises(RpcCallError) as e:
        call()
    code = e.value.code or ""
    assert "permission_denied" in code or "forbidden" in code or "not_allowed" in code, (
        f"{what} must be refused as not permitted; got {code!r}: {e.value}"
    )


@pytest.mark.feature("spam")
def test_neither_another_user_nor_the_admin_reads_or_trains_a_users_model(
    nest_instance, seal_helper_binary,
):
    db_path = nest_instance["db_path"]
    other = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"])
    victim_id = _provision_msek_recipient(
        nest_instance=nest_instance, seal_helper_binary=seal_helper_binary,
        domain=None, local_part="spam-isolation-victim",
        password="spam-isolation-victim-password-1",
    ).actor_id
    _seed_spam_model(db_path=db_path, actor_id=victim_id, seal_helper_binary=seal_helper_binary,
                     ngrams={"qzisolationwx": (60, 0)}, spam_messages=60, ham_messages=60)
    [victim_hid] = _seed_spam_training_history(
        db_path=db_path, actor_id=victim_id, seal_helper_binary=seal_helper_binary, rows=[{
            "message_id": b"\x07" * 32, "mailbox": "Junk", "subject": "victim's lesson",
            "label": "spam", "source": "imap_junk_flag",
        }])
    before = _victim_state(db_path, victim_id)
    assert before[0] is not None, "precondition: the victim has a trained model"

    for who, open_ws in (("another user", lambda: _user_ws(nest_instance, other)),
                         ("the admin", lambda: _admin_ws(nest_instance))):
        with open_ws() as ws:
            _refused(lambda: ws.call("fauna.bridges.fetch_spam_model",
                                     {"actor_id": victim_id}),
                     f"{who} reading the victim's model")
            _refused(lambda: ws.call("fauna.bridges.put_spam_model",
                                     {"actor_id": victim_id, "sealed_model": b"\x00" * 64}),
                     f"{who} writing the victim's model")
            # The caller-scoped kinds carry no target: they act on the caller
            # alone — the victim's entry is never listed, an undo (a sealed
            # write's history delete) naming it matches nothing, and a reset
            # clears only the caller's own filter (which also drops the opaque
            # placeholder model the undo probe just wrote for the caller).
            events = ws.call("fauna.bridges.list_spam_training_history", {})["events"]
            assert victim_hid not in [e["history_id"] for e in events], (
                f"{who}'s history listing must not include the victim's entry")
            try:
                ws.call("fauna.bridges.put_spam_model", {
                    "sealed_model": b"\x00" * 64,
                    "history_op": {"Delete": {"history_id": victim_hid}},
                })
            except RpcCallError:
                pass  # refused or a no-op — either way, asserted at rest below
            ws.call("fauna.bridges.reset_spam_model", {})

    assert _victim_state(db_path, victim_id) == before, (
        "the victim's model bytes and training history must be untouched by every "
        "cross-actor probe, the admin's included")
