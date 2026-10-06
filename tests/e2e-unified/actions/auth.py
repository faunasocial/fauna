from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class AuthActions:
    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def login(self, node_url: str, username: str, password: str,
              secret_hex: str = "", handle: str = "", domain: str = "") -> None:
        """Land the app on the feed for tests that don't care about the
        onboarding UI.

        All apps are bridge-backed (`HttpBridgeDriver` subclass) and the
        test agent's `set_state` patch atomically seeds the authenticated
        session — same primitive `logged_in_app` uses. The 5-page legacy
        UI walk this helper used to do (`enter-address-button` /
        `nest-claim-handle-field` etc.) is gone with the handle-first
        rewrite (`docs/goal/behavior/onboarding.md`); no public IDs survived the
        migration so a UI-walking helper would need a complete rewrite
        per platform on top of bridge-only state seeding tests like
        `test_handle_entry_outcomes.py` already exercise.

        For tests that genuinely want to drive the new onboarding UI from
        scratch, use the `OnboardingActions` helper plus
        `driver.call_machine_method(...)` — see
        `tests/e2e-unified/tests/test_handle_entry_outcomes.py` for the
        bridge pattern.
        """
        from drivers.http_bridge import HttpBridgeDriver

        if not isinstance(self.driver, HttpBridgeDriver):
            raise RuntimeError(
                f"AuthActions.login requires a HttpBridgeDriver-based "
                f"client; got {type(self.driver).__name__}. The legacy "
                f"5-page UI walk for native apps was retired with the "
                f"handle-first onboarding rewrite — see "
                f"docs/goal/behavior/onboarding.md and the bridge contract in "
                f"the same doc's §'E2E bridge contract'."
            )

        self.driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": node_url,
                "secret_hex": secret_hex,
                "handle": handle or "e2e-user",
                "domain": domain or "",
                "actor_id": username,
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        self.driver.wait_for("feed-view", timeout=30.0)
