from __future__ import annotations

from typing import TYPE_CHECKING

from helpers.waiting import wait_until
from i18n.strings import S

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class RetireActions:
    """Drive the ``nest_retire`` page (docs/goal/behavior/nest-retirement.md).

    One page in the wizard family: credentials → list → confirm → running →
    done, every state the shared ``NestRetireMachine``'s. The credential state
    reuses ``vps_config``'s controls by id (``vps-provider-row[<id>]``,
    ``vps-credentials-form-<field>``, ``vps-verify-button``); the running state
    reuses ``provisioning-step-row`` (0 = DNS records, 1 = the server). A listed
    server is ``retire-server-item[n]``, its members scoped inside it.

    Entered from ``launch-retire-button`` (launch_retry) or
    ``admin-nest-retire-button`` (admin-nest). The provider base-URL override
    (``set_provider_base_urls``) must be installed BEFORE the page is opened —
    the page's machine takes it at construction.
    """

    ROW = "retire-server-item"

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    # -- entries -----------------------------------------------------------

    def open_from_launch(self) -> None:
        self.driver.wait_for("launch-retire-button", timeout=60)
        self.driver.click("launch-retire-button")
        self.driver.wait_for("retire-back-button", timeout=15)

    def open_from_admin(self, admin) -> None:
        """``admin-nest-retire-button``, over the live session. ``admin`` is the
        ActionLayer's admin actions (its ``navigate_nest``)."""
        admin.navigate_nest()
        self.driver.wait_for("admin-nest-retire-button", timeout=30)
        self.driver.click("admin-nest-retire-button")
        self.driver.wait_for("retire-back-button", timeout=15)

    # -- credentials -------------------------------------------------------

    def enter_token(self, provider_id: str, token: str) -> None:
        """Pick a provider with a typed token field and verify it."""
        self.driver.click(f"vps-provider-row[{provider_id}]")
        self.driver.wait_for("vps-credentials-form-api-token", timeout=10)
        self.driver.fill("vps-credentials-form-api-token", token)
        self.verify()

    def sign_in_bundled(self, base_url: str) -> None:
        """The bundled provider: type its address, press the hosted sign-in
        button (the fake approves on the first poll), then verify."""
        self.driver.click("vps-provider-row[bundled]")
        self.driver.wait_for("vps-credentials-form-base-url", timeout=10)
        self.driver.fill("vps-credentials-form-base-url", base_url)
        self.driver.click("vps-credentials-form-api-token")
        wait_until(
            lambda: self.driver.get_text("vps-credentials-form-api-token").strip()
            == S.provisioning.hosted_auth.connected,
            30,
            diagnose=lambda: "the hosted sign-in never connected: "
            + self.driver.get_text("vps-credentials-form-api-token"),
        )
        self.verify()

    def verify(self) -> None:
        # Verify stays dead until every input the page awaits has landed (the
        # admin entry's live account-plane read) — wait for the state, then press.
        wait_until(
            lambda: self.driver.is_enabled("vps-verify-button"),
            30,
            diagnose=lambda: "verify never became pressable: "
            + self.driver.diagnose("vps-verify-button"),
        )
        self.driver.click("vps-verify-button")
        wait_until(
            lambda: self.driver.count(self.ROW) > 0
            or self.driver.is_visible("retire-server-empty-message"),
            30,
            diagnose=lambda: "the list never arrived: "
            + self.driver.diagnose("error-message"),
        )

    # -- list --------------------------------------------------------------

    def row_names(self) -> list[str]:
        return [
            self.driver.get_text("retire-server-name", scope=f"{self.ROW}[{i}]")
            for i in range(self.driver.count(self.ROW))
        ]

    def row_scope(self, name: str) -> str:
        names = self.row_names()
        assert name in names, f"no listed server named {name!r}; the list is {names!r}"
        return f"{self.ROW}[{names.index(name)}]"

    def select(self, name: str) -> str:
        """Select the named row; returns its scope."""
        names = self.row_names()
        assert name in names, f"no listed server named {name!r}; the list is {names!r}"
        self.driver.click(self.ROW, names.index(name))
        wait_until(
            lambda: self.driver.is_enabled("retire-delete-button"),
            10,
            diagnose=lambda: self.driver.diagnose("retire-delete-button"),
        )
        return f"{self.ROW}[{names.index(name)}]"

    # -- confirm -----------------------------------------------------------

    def begin_confirm(self) -> None:
        self.driver.click("retire-delete-button")
        self.driver.wait_for("retire-confirm-name-input", timeout=10)

    def type_name(self, typed: str) -> None:
        self.driver.fill("retire-confirm-name-input", typed)

    def confirm(self) -> None:
        self.driver.click("retire-confirm-button")

    # -- running / done ----------------------------------------------------

    def wait_done(self, timeout: float = 30) -> None:
        self.driver.wait_for("retire-done-button", timeout=timeout)

    def wait_dns_failed(self, timeout: float = 30) -> None:
        self.driver.wait_for("retire-force-server-button", timeout=timeout)

    def step_error(self, index: int) -> str | None:
        scope = f"provisioning-step-row[{index}]"
        if self.driver.is_absent("provisioning-step-error", scope=scope):
            return None
        return self.driver.get_text("provisioning-step-error", scope=scope)
