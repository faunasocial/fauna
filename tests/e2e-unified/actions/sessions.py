from __future__ import annotations

from typing import TYPE_CHECKING

from helpers.waiting import wait_until

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class SessionsActions:
    """Drive the Settings → Sessions sub-page (``docs/goal/ui/sessions.md``;
    ui.yaml ``sessions`` page) — where the account is signed in right now:
    ``session-card`` rows (``session-kind`` / ``session-detail`` /
    ``session-this-mark-badge`` / ``session-revoke-button``), sign out everywhere
    else (``sessions-revoke-others-*``, a two-press inline confirm) and the
    24-hour lock (``sessions-lockout-*``, type the literal ``LOCK``).

    Every app renders the rows off the one shared fold
    (``fauna_client_account::sessions_view``), so the order is the contract:
    this app's own row is ``session-card[0]`` and carries no revoke button.
    """

    #: A settings sub-page nav is a fire-and-forget ``set_state`` patch, and the
    #: page's hydrate is a network read plus a roster refresh. A generous
    #: positive budget (convention 14) — a green run exits the moment it lands.
    PAGE_READY_BUDGET_S = 60.0

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self, *, timeout: float | None = None) -> None:
        """Open the page and wait until its hydrate has folded — the painted
        token-id list is non-null only once a snapshot landed."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "sessions"}]},
        })
        self.driver.wait_for(
            "sessions-revoke-others-button", timeout=timeout or self.PAGE_READY_BUDGET_S
        )
        wait_until(
            lambda: self.painted_token_ids() is not None,
            timeout or self.PAGE_READY_BUDGET_S,
            interval=0.25,
            diagnose=lambda: (
                "the Sessions page never folded a snapshot "
                f"(error-message: {self.error_text()!r})"
            ),
        )

    # ── reads ─────────────────────────────────────────────────────────────
    def painted_token_ids(self) -> list[str] | None:
        """The token id behind each painted ``session-card``, in order —
        ``None`` before the first hydrate (the app's ``settings`` state key)."""
        settings = self.driver.get_state("settings")
        if not isinstance(settings, dict):
            return None
        return settings.get("session_token_ids")

    def card_count(self) -> int:
        return self.driver.count("session-card")

    def kind(self, index: int) -> str:
        return self.driver.get_text("session-kind", scope=f"session-card[{index}]")

    def mark(self, index: int) -> str | None:
        """The card's ``session-this-mark-badge`` text, or ``None`` when absent."""
        scope = f"session-card[{index}]"
        if self.driver.is_absent("session-this-mark-badge", scope=scope):
            return None
        return self.driver.get_text("session-this-mark-badge", scope=scope)

    def has_revoke_button(self, index: int) -> bool:
        return not self.driver.is_absent("session-revoke-button", scope=f"session-card[{index}]")

    def error_text(self) -> str:
        if self.driver.is_absent("error-message"):
            return ""
        return self.driver.get_text("error-message")

    # ── acts (through the UI — convention 8) ──────────────────────────────
    def revoke(self, index: int) -> None:
        self.driver.click("session-revoke-button", scope=f"session-card[{index}]")

    def sign_out_everywhere_else(self) -> None:
        """Both presses of the inline confirm."""
        self.driver.click("sessions-revoke-others-button")
        self.driver.wait_for("sessions-revoke-others-confirm-button", timeout=10.0)
        self.driver.click("sessions-revoke-others-confirm-button")

    def lock(self, word: str = "LOCK") -> None:
        self.driver.fill("sessions-lockout-confirm-field", word)
        self.driver.click("sessions-lockout-button")

    # ── the locked surface (leg 2; ui.yaml `launch_account_locked`) ───────
    #: The lock lands, the app leaves the shell and re-runs its launch
    #: challenge, and the nest's refusal parks the launch machine. Generous by
    #: design (convention 14); a green run exits the moment the notice paints.
    LOCKED_SURFACE_BUDGET_S = 120.0

    def wait_for_locked_notice(self) -> str:
        """Wait for the standing ``launch-account-locked-notice``; return its
        text. The surface's own element — never ``error-message`` — so no other
        refusal can satisfy the wait."""

        def _notice() -> str:
            if self.driver.is_absent("launch-account-locked-notice"):
                return ""
            return self.driver.get_text("launch-account-locked-notice")

        # Convention 6: a lock the app never heard land leaves it on the
        # Sessions page with the failure on `error-message` — say so, rather
        # than report a bare missing element.
        return wait_until(
            _notice,
            self.LOCKED_SURFACE_BUDGET_S,
            diagnose=lambda: (
                "the locked notice never painted "
                f"(error-message: {self.error_text()!r}, "
                f"still on the Sessions page: "
                f"{not self.driver.is_absent('sessions-lockout-button')})"
            ),
        )

    def open_stolen_entry(self) -> None:
        """The locked surface's one action → the ``identity_stolen_entry`` step."""
        self.driver.click("launch-account-locked-stolen-button")
        self.driver.wait_for("identity-stolen-button", timeout=30.0)

    def succeed_from_locked(self, phrase: str) -> None:
        """Run the stolen-identity ceremony from ``identity_stolen_entry`` with
        the kit in hand — the same irreversible act
        ``SettingsActions.succeed_identity_with_held_kit`` drives, from outside
        a session, and refused on a shared identity for the same reason."""
        from helpers.shared_identity import refuse_ceremony_on_shared_identity

        refuse_ceremony_on_shared_identity(self.driver, ceremony="succeed_from_locked")
        self.driver.fill("recovery-entry-phrase-field", phrase)
        self.driver.fill("identity-stolen-confirm-field", "SUCCEED")
        self.driver.click("identity-stolen-button")
