"""The harness's backup-leg exports are bound and decode before they dial.

Not a witness of any leg (the api-tier smoke ``tests/api/test_harness_backup_legs_api.py``
drives them against real nests): a library missing an export, or bound with
the wrong argument types, fails here in process, with no nest started."""
import pytest

import fauna_ffi

pytestmark = pytest.mark.tier_1

# A port nothing listens on: an export that dialled before decoding would
# fail with a connect error rather than the decode error asserted below.
_NOWHERE = "http://127.0.0.1:1"
_SECRET = b"\x07" * 32


@pytest.mark.parametrize("export", fauna_ffi._BACKUP_EXPORTS)
def test_each_backup_leg_export_is_bound(export):
    assert hasattr(fauna_ffi._lib, export), (
        f"libfauna_ffi has no `{export}` — stale, or not the `e2e-harness` "
        "flavor (`just e2e-ffi`)"
    )


@pytest.mark.parametrize("export", fauna_ffi._BACKUP_EXPORTS)
def test_a_malformed_request_fails_locally_before_any_dial(export):
    # Every leg's request is an array; a bare integer is none of them.
    with pytest.raises(RuntimeError, match="decode request"):
        fauna_ffi._seat_call(export, _NOWHERE, _SECRET, 7)


def test_the_wrappers_refuse_a_short_secret():
    with pytest.raises(ValueError):
        fauna_ffi.harness_backup_audit_pass(_NOWHERE, b"\x01" * 16, "/nonexistent")
