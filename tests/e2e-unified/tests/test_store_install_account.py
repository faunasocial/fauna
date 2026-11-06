"""The account a store-install witness signs in with
(``helpers/store_install_account.py``): this machine's own tester when it is
minted here and registered, else the store's reviewer where the drive keeps
it, else an environment skip naming both routes — `org-nests.md`
§ fauna.build → *Accounts*, the witness-host tester.

Test taxonomy: tier_1 — pure resolution over a temporary state dir and
registry.
"""

from __future__ import annotations

import json

import pytest

from helpers import staging_box as sb
from helpers import store_install_account as sia

pytestmark = pytest.mark.tier_1

BOX = "test.fauna.build"


@pytest.fixture
def home(tmp_path, monkeypatch):
    monkeypatch.setattr(sb, "STATE_DIR", str(tmp_path / "staging-box"))
    monkeypatch.setattr(sb, "WITNESS_TESTER_REGISTRY", tmp_path / "org-testers.toml")
    return tmp_path


def _skip_reason(fn, *args) -> str:
    with pytest.raises(pytest.skip.Exception) as e:
        fn(*args)
    return str(e.value)


def test_this_machines_registered_tester_is_the_witness_account(home):
    sb.register_witness_tester(BOX, now="2026-11-05T00:00:00Z")
    tester = sb.witness_tester_localpart()
    account = sia.witness_account(sia.APP_STORE_ACCOUNT)
    assert account.handle == f"{tester}@{BOX}" and account.localpart == tester
    assert account.secret_hex == sb.load_or_create_account(BOX, tester)["secret_hex"]
    assert account.secret_hex not in repr(account)


def test_the_reviewer_stands_in_where_no_tester_is_minted(home):
    # The dev host: the drive keeps the reviewers' identities there.
    reviewer = sb.load_or_create_account(BOX, sia.APP_STORE_ACCOUNT)
    account = sia.witness_account(sia.APP_STORE_ACCOUNT)
    assert account.handle == f"{sia.APP_STORE_ACCOUNT}@{BOX}"
    assert account.secret_hex == reviewer["secret_hex"]


def test_a_minted_but_unregistered_tester_is_not_signed_in_as(home):
    # Admitted nowhere: the reviewer stands in where kept, else a skip naming
    # the recipe and the registry.
    tester = sb.witness_tester_localpart()
    sb.load_or_create_account(BOX, tester)
    reason = _skip_reason(sia.witness_account, sia.PLAY_ACCOUNT)
    assert "minted but not registered" in reason and "org-tester-mint" in reason
    assert f"{sia.PLAY_ACCOUNT}.json" in reason
    sb.load_or_create_account(BOX, sia.PLAY_ACCOUNT)
    assert sia.witness_account(sia.PLAY_ACCOUNT).localpart == sia.PLAY_ACCOUNT


def test_nothing_minted_skips_naming_both_routes(home):
    reason = _skip_reason(sia.witness_account, sia.APP_STORE_ACCOUNT)
    assert "not minted" in reason and "org-tester-mint" in reason
    assert "the dev host alone" in reason


def test_a_registered_key_that_is_not_the_kept_identity_skips(home):
    # The registry names another machine's identity under this host's handle:
    # signing in with the kept seed would be refused, so the witness says
    # what to re-run instead of failing on the device.
    sb.register_witness_tester(BOX, now="2026-11-05T00:00:00Z")
    tester = sb.witness_tester_localpart()
    path = sb.account_path(BOX, tester)
    path.write_text(json.dumps({"domain": BOX, "localpart": tester, "secret_hex": "22" * 32}))
    reason = _skip_reason(sia.witness_account, sia.APP_STORE_ACCOUNT)
    assert "not the identity registered" in reason and "org-tester-mint" in reason


def test_a_malformed_identity_file_skips_naming_it(home):
    path = sb.account_path(BOX, sia.APP_STORE_ACCOUNT)
    path.parent.mkdir(parents=True)
    path.write_text(json.dumps({"domain": BOX, "localpart": "tester", "secret_hex": "ab"}))
    reason = _skip_reason(sia.witness_account, sia.APP_STORE_ACCOUNT)
    assert str(path) in reason and "64-hex seed" in reason
