"""tier_3 e2e for the user-tier alias bulk-import RPC
(``fauna.bridges.import_account_aliases`` — Plan 1 of the
recipient-whitelist/alias-import design, tracked internally).

Bulk-creates **exact** aliases from a list of full addresses (one per line),
returning a per-line ``ImportAliasOutcome`` (``created`` / ``skipped_duplicate``
/ ``invalid`` + reason). Best-effort: a malformed / domain-not-local /
over-cap / duplicate line is *reported*, never aborting the batch. Idempotent:
re-importing an existing address is ``skipped_duplicate``.

Unlike the single ``create_account_alias`` (whose e2e in ``test_mail_aliases_user.py``
uses a synthetic domain because create does **not** validate domain existence),
import **does** validate domain-ownership against ``mail_domains``, so this test
provisions a real local domain via the admin ``add_local_domain`` RPC (the same
seeding ``test_mail_forwarder.py`` uses). Local-parts are randomized so the
cross-user ``UNIQUE(local_domain, pattern, kind)`` index can't turn a re-run's
``created`` into ``skipped_duplicate`` on the session-scoped nest.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register

pytestmark = pytest.mark.tier_3

DOMAIN = "import-e2e.test"


def _admin_client(nest_instance):
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _user_client(nest_instance, user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )


def _ensure_domain(admin):
    """Idempotently make the nest own ``DOMAIN`` (re-add is a no-op)."""
    admin.call(
        "fauna.bridges.add_local_domain",
        {
            "domain": DOMAIN,
            "mta_sts_cert_mode": "expand_primary",
        },
    )


def _fresh_user(nest_instance):
    return create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )


def _list_patterns(client):
    rows = client.call("fauna.bridges.list_account_aliases", {})["aliases"]
    return {r["pattern"] for r in rows}


@pytest.mark.feature("mail-aliases")
def test_import_account_aliases_mixed_batch(nest_instance):
    with _admin_client(nest_instance) as admin:
        _ensure_domain(admin)

    p1 = "me-netflix-" + secrets.token_hex(4)
    p2 = "me-amazon-" + secrets.token_hex(4)
    lines = [
        f"{p1}@{DOMAIN}",       # 0 -> created
        f"{p2}@{DOMAIN}",       # 1 -> created
        "",                      # 2 -> skipped (no outcome)
        "bad@notmine.test",      # 3 -> invalid: domain not local
        "notanemail",            # 4 -> invalid: malformed address
        f"{p1}@{DOMAIN}",       # 5 -> skipped_duplicate (same as line 0)
    ]

    user = _fresh_user(nest_instance)
    with _user_client(nest_instance, user) as client:
        reply = client.call(
            "fauna.bridges.import_account_aliases", {"lines": lines}
        )
        outcomes = {o["line_index"]: o for o in reply["results"]}

        assert outcomes[0]["status"] == "created"
        assert outcomes[1]["status"] == "created"
        assert 2 not in outcomes, "blank line must produce no outcome"
        assert outcomes[3]["status"] == "invalid"
        assert outcomes[3]["reason"] == "domain not local"
        assert outcomes[4]["status"] == "invalid"
        assert outcomes[4]["reason"] == "malformed address"
        assert outcomes[5]["status"] == "skipped_duplicate"

        # The two created aliases now exist (owner-scoped list).
        patterns = _list_patterns(client)
        assert p1 in patterns
        assert p2 in patterns
