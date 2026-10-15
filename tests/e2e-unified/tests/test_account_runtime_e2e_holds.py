"""tier_1: the account runtime's e2e holds are spelled the same on both sides.

A journey sets ``FAUNA_E2E_HOLD_PUBLISH_KIND`` / ``FAUNA_E2E_HOLD_FLEET_WALK``
through ``helpers/account_runtime_holds.py``; the runtime reads them in
``libs/fauna-account-plane/src/account_driver/e2e_holds.rs``. A drifted
spelling would not fail the journey — the hold would simply never arm, and the
journey would go back to passing on a tree that leans on the runtime's publish
or walk latency (a false green, `mail-credentials.md` § Hard revoke →
*Published before the nest moves*). The gate itself — compiled out of release
builds — is ``test_shared_crate_seam_gating.py``'s env-seed scan.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from helpers.account_runtime_holds import KIND_MAIL, PUBLISH_HOLD_ENV, WALK_HOLD_ENV

pytestmark = [pytest.mark.tier_1]

_REPO = Path(__file__).resolve().parents[3]
_HOLDS_RS = _REPO / "libs/fauna-account-plane/src/account_driver/e2e_holds.rs"
_MERGE_POLICY_RS = _REPO / "libs/fauna-protocol/src/merge_policy.rs"


def test_the_hold_variables_are_the_ones_the_runtime_reads():
    source = _HOLDS_RS.read_text()
    for name, spelling in (
        ("PUBLISH_HOLD_ENV", PUBLISH_HOLD_ENV),
        ("WALK_HOLD_ENV", WALK_HOLD_ENV),
    ):
        assert f'const {name}: &str = "{spelling}";' in source, (
            f"{_HOLDS_RS.name} no longer declares {name} = {spelling!r}; a journey "
            "setting the Python spelling would hold nothing"
        )


def test_the_mail_kind_is_the_protocols():
    assert f'pub const KIND_MAIL: &str = "{KIND_MAIL}";' in _MERGE_POLICY_RS.read_text(), (
        f"fauna_protocol::merge_policy::KIND_MAIL is no longer {KIND_MAIL!r}; the "
        "publish hold keyed on it would never find an owed mail row"
    )
