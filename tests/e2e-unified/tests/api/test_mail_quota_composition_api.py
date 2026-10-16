"""tier_3 — what counts against a mailbox's storage quota, witnessed from outside.

Two `[nest]` outcomes about the deployment-wide mailbox storage quota
(``mail_imap_policy.storage_bytes_default``, written by the production admin
call ``fauna.bridges.put_imap_policy``), each on a nest of its own because the
policy is nest-wide and would reach every sibling test on the session nest:

* ``mailbox-import`` **46** — imported mail counts against your mailbox storage
  (``mailbox-migration.md`` § Quota composition): once your mail fills the quota,
  the next message is answered ``skipped:quota_exceeded``, per message, not as a
  failed call.
* ``mailbox-export`` **34** — exporting never counts against your mailbox storage
  (``mail-export.md`` § Quota composition): an export whose chunks alone exceed
  the quota uploads and finishes, and the mailbox still takes mail afterwards.

``get_quota`` is served to the MDA only, so the quota is observed by behaviour —
by what the nest accepts — not by reading a counter.
"""

from __future__ import annotations

import pytest

from conftest import _make_user, _start_dedicated_nest
from helpers.admin_wire import admin_client
from helpers.recipient_seal_key import provision_recipient_seal_key
from tests.api.test_mail_export_nest_api import _ws, _start, _finish
from tests.api.test_mail_import_nest_api import IMPORTED, _import, _item, _raw, _session

pytestmark = pytest.mark.tier_3

#: Roughly four KiB of body per message; the quota below holds one such message
#: (sealed, it is a little over four KiB) and not two.
PAD = "x" * 4000 + "\r\n"
QUOTA_BYTES = 8000
QUOTA_EXCEEDED = "skipped:quota_exceeded"


@pytest.fixture
def quota_nest(request, nest_mode, tmp_path_factory):
    """A nest of this test's own, its storage quota lowered through the production admin call."""
    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "quota-nest")
    try:
        with admin_client(nest) as admin:
            admin.call("fauna.bridges.put_imap_policy", {"storage_bytes_default": QUOTA_BYTES})
        yield nest
    finally:
        cleanup()


def _member(nest) -> dict:
    user = _make_user(nest)
    with _ws(nest, user) as ws:
        provision_recipient_seal_key(ws, user["actor_id_bytes"])
    return user


def _big(tag: str) -> tuple[bytes, str]:
    mid = f"{tag}@example.org"
    return _raw(mid, subject=f"quota {tag}", footer=PAD), mid


# ── import 46: imported mail counts against your mailbox storage ─────────────


@pytest.mark.feature("mailbox-import")
def test_imported_mail_counts_against_your_mailbox_storage(quota_nest):
    user = _member(quota_nest)
    first, first_id = _big("quota-first")
    second, second_id = _big("quota-second")
    with _ws(quota_nest, user) as ws:
        sid = _session(ws)
        assert _import(ws, sid, _item(first, first_id, uid=1)) == IMPORTED, (
            "the control: one message fits under the lowered quota"
        )
        answer = _import(ws, sid, _item(second, second_id, uid=2))
    assert answer == QUOTA_EXCEEDED, (
        "the first import took up the room, so a different second message must be turned "
        f"away for lack of it — as that message's own answer: {answer!r}"
    )


# ── export 34: exporting never counts against your mailbox storage ───────────


@pytest.mark.feature("mailbox-export")
def test_exporting_never_counts_against_your_mailbox_storage(quota_nest):
    user = _member(quota_nest)
    first, first_id = _big("quota-after-export")
    with _ws(quota_nest, user) as ws:
        session_id = _start(ws)
        finished = _finish(ws, session_id, payload=b"s" * (QUOTA_BYTES * 3))
        assert finished["state"] != "failed", (
            f"an export bigger than the whole mailbox quota must still finish: {finished!r}"
        )
        answer = _import(ws, _session(ws), _item(first, first_id))
    assert answer == IMPORTED, (
        "export bytes must not be charged to the mailbox: with an export three times the quota "
        f"on file, a message that fits alone must still be accepted: {answer!r}"
    )
