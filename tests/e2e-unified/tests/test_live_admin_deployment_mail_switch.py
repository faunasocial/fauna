"""tier_1: enabling mail on a live box turns on the DEPLOYMENT switch, not only
the admin's own mailbox.

Mail on a box has two halves (`mail-bridge-lifecycle.md` § Default-off on
first claim): the admin's own mail credential (the mail-settings page) and the
deployment-wide switch `admin-mail-enabled-toggle` (`set_mail_enabled`), whose
persisted state is what brings the MTA/MDA bridge services up. An unset switch
reads OFF. `helpers.live_admin.enable_mail` used to stop at the first half: when
the credential already existed it returned at once, so a box whose switch was
off kept every mail port closed for good, and every resume of the staging-box
drive went red at its mail gate (test.fauna.build, measured 2026-11-10: the
admin's mail health read "Mail: off" while the credential existed; 25, 465, 587
and 993 refused).

Pinned here, headlessly with a fake driver:

  1. A credential already present and the switch OFF → the switch is clicked
     once, through the admin Mail page reached by clicks, and its state is
     awaited ON.
  2. The switch already ON → no click.
  3. A box that cannot enable mail (`unavailable`) fails with the page's notice
     instead of clicking a disabled control.
"""

from __future__ import annotations

import pytest

from helpers import live_admin

pytestmark = pytest.mark.tier_1


class _FakeDriver:
    """Compares against `live_admin`'s own constants, never an element-ID
    literal: the nest-mode scan merges public methods by bare name, so a
    literal in this `click` would put the admin Mail page on every journey that
    clicks (`test_nest_mode_axis.py::test_no_driver_method_name_reaches_an_admin_element`)."""

    def __init__(self, switch: str):
        self.switch = switch
        self.clicks: list[str] = []

    def count(self, element_id: str) -> int:
        return 1

    def get_attr(self, element_id: str, attr: str):
        assert (element_id, attr) == (live_admin._MAIL_SWITCH, "state")
        return self.switch

    def get_text(self, element_id: str) -> str:
        return {live_admin._MAIL_NOTICE: "this box has no content scanners"}.get(
            element_id, "")

    def click(self, element_id: str, index: int | None = None) -> None:
        self.clicks.append(element_id)
        if element_id == live_admin._MAIL_SWITCH:
            self.switch = "on" if self.switch == "off" else "off"


class _FakeAdmin:
    def __init__(self):
        self.opened: list[str] = []

    def open_page_by_click(self, page_id: str, canary: str, timeout: float = 15.0) -> None:
        self.opened.append(page_id)


class _FakeApp:
    def __init__(self, switch: str):
        self.driver = _FakeDriver(switch)
        self.admin = _FakeAdmin()

    def error_text(self) -> str:
        return ""


def test_an_off_switch_is_turned_on_through_the_admin_mail_page():
    app = _FakeApp("off")
    assert live_admin.ensure_deployment_mail_on(app, timeout=5.0) is True
    assert app.admin.opened == ["admin-mail"]
    assert app.driver.clicks == ["admin-mail-enabled-toggle"]
    assert app.driver.switch == "on"


def test_an_on_switch_is_left_alone():
    app = _FakeApp("on")
    assert live_admin.ensure_deployment_mail_on(app, timeout=5.0) is False
    assert app.driver.clicks == []


def test_a_box_that_cannot_enable_mail_fails_with_its_notice():
    app = _FakeApp("unavailable")
    with pytest.raises(pytest.fail.Exception, match="no content scanners"):
        live_admin.ensure_deployment_mail_on(app, timeout=5.0)
    assert app.driver.clicks == []


def test_a_resume_with_no_stored_password_still_turns_the_switch_on():
    """The drive's resume on a box whose admin credential it did not mint holds
    no password (test.fauna.build, 2026-11-10): the switch is still turned on."""
    app = _FakeApp("off")
    assert live_admin.bring_box_mail_up(app, None) is False
    assert app.driver.switch == "on"


def test_enable_mail_with_a_credential_still_checks_the_switch(monkeypatch):
    app = _FakeApp("off")

    class _MailSettings:
        def open_by_click(self):
            pass

        def wait_for_credential_count_at_least(self, n, timeout):
            return True

    app.mail_settings = _MailSettings()
    assert live_admin.enable_mail(app, "pw") is False  # the credential was not minted here
    assert app.driver.switch == "on"
