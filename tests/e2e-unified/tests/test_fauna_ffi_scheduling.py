"""The harness's scheduling exports are bound and decode before they dial.

Not a witness of any outcome (``tests/api/test_scheduling_client_authority.py``
drives them against a real nest): a library missing an export, or bound with
the wrong argument types, fails here in process, with no nest started."""
import pytest

import fauna_ffi

pytestmark = pytest.mark.tier_1

# A port nothing listens on: an export that dialled before decoding would
# fail with a connect error rather than the decode error asserted below.
_NOWHERE = "http://127.0.0.1:1"
_SECRET = b"\x07" * 32


@pytest.mark.parametrize("export", fauna_ffi._SCHEDULING_EXPORTS)
def test_each_scheduling_export_is_bound(export):
    assert hasattr(fauna_ffi._lib, export), (
        f"libfauna_ffi has no `{export}` — stale, or not the `e2e-harness` "
        "flavor (`just e2e-ffi`)"
    )


@pytest.mark.parametrize("export", fauna_ffi._SCHEDULING_EXPORTS)
def test_a_malformed_request_fails_locally_before_any_dial(export):
    # Every export's request is an array; a bare integer is none of them.
    with pytest.raises(RuntimeError, match="decode request"):
        fauna_ffi._scheduling_call(export, _NOWHERE, _SECRET, 7)


def test_an_unknown_method_or_rail_is_refused_before_any_dial():
    with pytest.raises(RuntimeError, match="REQUEST or CANCEL"):
        fauna_ffi.harness_scheduling_invite(
            _NOWHERE, _SECRET, _SECRET, b"\x01" * 32, method="REPLY", uid="u",
            summary="s", organizer="a@fauna.test", attendees=["b@fauna.test"], directory={},
        )
    with pytest.raises(RuntimeError, match="rail must be"):
        fauna_ffi.harness_scheduling_rsvp(
            _NOWHERE, _SECRET, b"\x01" * 32, _SECRET, b"\x02" * 32, uid="u",
            as_address="b@fauna.test", rail="carrier-pigeon", directory={},
        )
