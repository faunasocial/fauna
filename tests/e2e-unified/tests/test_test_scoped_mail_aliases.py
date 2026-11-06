"""tier_1: an inbound address a test mints is released at its teardown, and nothing else is.

Owner: ``docs/goal/architecture/e2e-launch-isolation.md`` (convention 10: a
test's outcome must not depend on what ran before it). The session ``test_user``
is shared by every mail test and the nest caps a member's exact aliases, so the
address each test routes inbound mail to (``route_inbound_mail_to_app``) must
not outlive the test — else a long mail module runs its later tests into
``fauna.bridges.alias_cap_exceeded``. The seam is
``helpers/mail_aliases.py::add_test_scoped_exact_alias`` and
``release_test_scoped_aliases``. Pure state against a fake nest.
"""

import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

import helpers.mail_aliases as mail_aliases
from clients.ws_rpc_admin_client import RpcCallError

pytestmark = pytest.mark.tier_1


class _Key:
    verify_key = b"v" * 32

    def __bytes__(self):
        return b"s" * 32


class _Nest:
    """One member's alias table, as the two alias doors see it."""

    def __init__(self, rows):
        self.rows = list(rows)
        self.next_id = 100

    def call(self, kind, payload):
        if kind == "fauna.bridges.list_account_aliases":
            return {"aliases": [dict(r) for r in self.rows]}
        if kind == "fauna.bridges.create_account_alias":
            if any(r["pattern"] == payload["pattern"]
                   and r["local_domain"] == payload["local_domain"] for r in self.rows):
                raise RpcCallError(
                    "fauna.bridges.conflicts_with_existing_alias", "k", {}, None)
            self.next_id += 1
            self.rows.append({
                "alias_id": self.next_id, "kind": "exact",
                "local_domain": payload["local_domain"], "pattern": payload["pattern"],
                "disabled": False, "is_canonical": False,
            })
            return {}
        if kind == "fauna.bridges.delete_account_alias":
            self.rows = [r for r in self.rows if r["alias_id"] != payload["alias_id"]]
            return {}
        raise AssertionError(kind)


@pytest.fixture
def nest(monkeypatch):
    nest = _Nest([{
        "alias_id": 1, "kind": "exact", "local_domain": "fauna.test",
        "pattern": "alice", "disabled": False, "is_canonical": True,
    }, {
        "alias_id": 2, "kind": "exact", "local_domain": "fauna.test",
        "pattern": "seeded", "disabled": False, "is_canonical": False,
    }])

    class _Client:
        def __init__(self, *_a, **_kw):
            pass

        def __enter__(self):
            return nest

        def __exit__(self, *_exc):
            return False

    monkeypatch.setattr(mail_aliases, "WsRpcAdminClient", _Client)
    monkeypatch.setattr(mail_aliases, "_TEST_SCOPED", [])
    return nest


def _patterns(nest):
    return sorted(r["pattern"] for r in nest.rows)


def test_a_minted_address_is_released_and_the_member_is_left_as_found(nest):
    before = _patterns(nest)
    addr = mail_aliases.add_test_scoped_exact_alias("ws://n", _Key(), "fauna.test", "e2eexport")
    assert addr == "e2eexport@fauna.test"
    assert "e2eexport" in _patterns(nest)
    mail_aliases.release_test_scoped_aliases()
    assert _patterns(nest) == before


def test_an_address_the_member_already_held_is_never_released(nest):
    for local in ("alice", "seeded"):
        mail_aliases.add_test_scoped_exact_alias("ws://n", _Key(), "fauna.test", local)
    mail_aliases.release_test_scoped_aliases()
    assert _patterns(nest) == ["alice", "seeded"]


def test_more_tests_than_the_cap_never_accumulate(nest):
    # Each "test" mints its own address and its teardown releases it, so the
    # table never grows past one test's worth — the whole-file run's shape.
    for i in range(30):
        mail_aliases.add_test_scoped_exact_alias("ws://n", _Key(), "fauna.test", f"t{i}")
        assert len(nest.rows) == 3
        mail_aliases.release_test_scoped_aliases()
    assert len(nest.rows) == 2


def test_a_release_that_fails_warns_naming_the_address_and_does_not_raise(nest, monkeypatch):
    mail_aliases.add_test_scoped_exact_alias("ws://n", _Key(), "fauna.test", "gone")

    def _down(*_a, **_kw):
        raise ConnectionError("nest stopped")

    monkeypatch.setattr(mail_aliases, "WsRpcAdminClient", _down)
    with pytest.warns(UserWarning, match="gone@fauna.test"):
        mail_aliases.release_test_scoped_aliases()
    assert mail_aliases._TEST_SCOPED == []
