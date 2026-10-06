import pytest

pytestmark = pytest.mark.tier_1


def test_roundtrip_helper_exposes_public_api():
    from helpers import caldav_roundtrip as r

    for name in (
        "caldav_has",
        "caldav_lacks",
        "native_has",
        "native_lacks",
        "wait_caldav_serving",
        "wait_calendars_ready",
        "run_create_visibility_matrix",
    ):
        assert callable(getattr(r, name)), name
