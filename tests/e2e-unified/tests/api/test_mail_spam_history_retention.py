"""tier_3: a spam-training entry is kept for a month; after the retention sweep
it is gone and can no longer be undone, but what it taught the filter stays
(`docs/goal/behavior/mail-spam.md` § Training-sample retention), witnessed over
the real WS-RPC wire.

The sweep is the production `run_spam_training_history_gc`, run now through the
`test-hooks` endpoint `POST /api/v1/test/spam/history-gc/run` instead of at its
daily 03:00 slot; one entry is made a month and a day old by writing its
`created_at`, the one thing a tier_3 cannot wait out.

The model and every history row rest SEALED to the user's own recipient key, and
undo runs where that key is — in the user's app, which unwraps the row's delta,
applies the inverse to its model, re-seals it and writes it together with the
row's delete in one `put_spam_model` (`mail-spam.md` § Undo). This test stands in
for that app: it knows the plaintext it seeded, so it builds the inverted model
itself and seals it with the test-only seal-helper. The nest can never read the
model, so "what the gone entry taught stays" is witnessed at rest as the sweep
leaving the stored model byte-identical. A fresh entry beside the aged one is the
control: it survives the sweep and its undo still lands (the row goes, the
re-sealed model is stored), so "the aged entry cannot be undone" is not a dead
undo path. Synchronous calls, settled reads — convention 14.
"""

import json
import secrets
import sqlite3
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from conftest import (
    _actor_recipient_pubkey,
    _provision_msek_recipient,
    _seal_to_actor,
    _seed_spam_model,
    _seed_spam_training_history,
)

pytestmark = pytest.mark.tier_3

DAY_MS = 24 * 3600 * 1000


def _age_entry(db_path, history_id, created_at_ms):
    """Back-date one seeded entry — the one thing a tier_3 cannot wait out."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        conn.execute("UPDATE spam_training_history SET created_at = ? WHERE history_id = ?",
                     (created_at_ms, history_id))
        conn.commit()
    finally:
        conn.close()


def _model(db_path, actor_id):
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return bytes(conn.execute("SELECT model_json FROM spam_models WHERE actor_id = ?",
                                  (actor_id,)).fetchone()[0])
    finally:
        conn.close()


def _run_sweep(nest_url):
    import urllib.request

    req = urllib.request.Request(f"{nest_url}/api/v1/test/spam/history-gc/run", data=b"",
                                 headers={"Content-Type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=15.0) as resp:
        assert resp.status == 200, f"history-gc hook returned {resp.status}"


def _model_json(old_tok, new_tok, new_spam):
    return json.dumps({
        "version": 1,
        "ngrams": {old_tok: {"spam": 5, "ham": 0}, new_tok: {"spam": new_spam, "ham": 0}},
        "spam_messages": 5 + new_spam,
        "ham_messages": 10,
    }).encode()


@pytest.mark.feature("spam")
def test_training_history_kept_a_month_then_lessons_stay_but_cannot_be_undone(
    nest_instance, seal_helper_binary,
):
    db = nest_instance["db_path"]
    user = _provision_msek_recipient(
        nest_instance=nest_instance, seal_helper_binary=seal_helper_binary,
        domain=None, local_part="spam-retention", password="spam-retention-password-1",
    )
    actor_id = user.actor_id
    old_tok, new_tok = f"qzold{secrets.token_hex(3)}wx", f"qznew{secrets.token_hex(3)}wx"
    _seed_spam_model(db_path=db, actor_id=actor_id, seal_helper_binary=seal_helper_binary,
                     ngrams={old_tok: (5, 0), new_tok: (5, 0)}, spam_messages=10, ham_messages=10)
    aged, fresh = _seed_spam_training_history(
        db_path=db, actor_id=actor_id, seal_helper_binary=seal_helper_binary, rows=[
            {"message_id": secrets.token_bytes(32), "mailbox": "Junk", "subject": f"lesson {old_tok}",
             "label": "spam", "source": "imap_junk_flag", "delta": [old_tok]},
            {"message_id": secrets.token_bytes(32), "mailbox": "Junk", "subject": f"lesson {new_tok}",
             "label": "spam", "source": "imap_junk_flag", "delta": [new_tok]},
        ])
    _age_entry(db, aged, int(time.time() * 1000) - 31 * DAY_MS)
    taught = _model(db, actor_id)

    _run_sweep(nest_instance["url"])
    assert _model(db, actor_id) == taught, (
        "the sweep must leave the model untouched — what the gone entry taught stays")

    recipient_pubkey = _actor_recipient_pubkey(db_path=db, actor_id=actor_id)

    def _undo(ws, hid, inverted_model_json):
        """What the app's undo writes: the model with the entry's delta inverted,
        re-sealed to the user's own key, plus the entry's delete — atomically."""
        sealed = _seal_to_actor(seal_helper_binary=seal_helper_binary,
                                recipient_pubkey=recipient_pubkey, plaintext=inverted_model_json)
        ws.call("fauna.bridges.put_spam_model", {
            "sealed_model": sealed,
            "history_op": {"Delete": {"history_id": hid}},
        })
        return sealed

    with WsRpcAdminClient(nest_instance["url"], actor_id=actor_id,
                          signing_key=bytes(user.recipient["signing_key"])) as ws:
        listed = [e["history_id"] for e in
                  ws.call("fauna.bridges.list_spam_training_history", {})["events"]]
        # Gone from the list is what "can no longer be undone" means: the app's
        # undo needs the entry's sealed delta, and the sweep deleted it with the
        # entry — while the model it was folded into is untouched (asserted above).
        assert aged not in listed, "an entry older than a month must be gone after the sweep"
        assert fresh in listed, "an entry inside the month must survive the sweep"

        undone = _undo(ws, fresh, _model_json(old_tok, new_tok, new_spam=4))
        listed = [e["history_id"] for e in
                  ws.call("fauna.bridges.list_spam_training_history", {})["events"]]
        assert fresh not in listed, "control: an entry inside the window is still undoable"
        assert _model(db, actor_id) == undone, (
            "control: the undo's re-sealed model is what rests, stored verbatim")
