"""tier_1: every app seat signs in under its OWN device id.

The session nest and its test user are shared by every app in a run, and one
``sync_devices`` row carries one renewal grant: a second machine registering
its store principal onto the same named row replaces the first one's grant
(``sync-agent-credentials.md`` § Credential model — the store principal is the
row's only renewal credential). Two apps patched onto one fixed id are two
machines claiming one row, so each re-registration — a sign-in, a nest restart
voiding the other's latch — refuses the other's renewal and change records
(``fauna.auth.not_registered``, ``fauna.sync.signature_invalid``). That
ping-pong kept files-on-demand outcome 5 red on tui and linux; the share fixtures' recipient and stranger ids exist for the same
reason.
"""

import pytest

import conftest
from helpers.e2e_session import e2e_login_device_id

pytestmark = pytest.mark.tier_1

_APPS = ("tui", "web", "linux", "windows", "macos", "ios", "android")


def test_each_app_gets_a_distinct_login_device_id():
    ids = {app: e2e_login_device_id(app) for app in _APPS}
    assert len(set(ids.values())) == len(_APPS), ids


def test_a_login_device_id_is_32_byte_hex_and_stable():
    for app in _APPS:
        one = e2e_login_device_id(app)
        assert len(one) == 64 and int(one, 16) >= 0, one
        assert one == e2e_login_device_id(app)


def test_login_ids_never_collide_with_the_share_seats():
    seats = {conftest._E2E_SHARE_RECIPIENT_DEVICE_ID, conftest._E2E_SHARE_STRANGER_DEVICE_ID}
    assert not seats & {e2e_login_device_id(app) for app in _APPS}


def test_a_driver_resolves_to_its_app_name():
    class _Tui:
        def is_tui(self):
            return True

    assert e2e_login_device_id(_Tui()) == e2e_login_device_id("tui")
