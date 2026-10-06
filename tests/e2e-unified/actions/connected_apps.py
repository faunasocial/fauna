from __future__ import annotations

import time
from typing import TYPE_CHECKING

from helpers.app_surface import app_name
from helpers.budgets import RPC_ROUNDTRIP_S
from i18n.strings import S

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


# The Connected apps page is built on all 7 apps, so the rows it lifted (the
# AT Protocol consent card + connected-app rows, the Nostr bunker rows) render
# HERE and nowhere else
# (docs/goal/ui/connected-apps.md § Implementation status today).
#
# The apps whose roster also lists the mail app passwords — moved off Mail &
# Calendar (docs/goal/ui/mail-settings.md § Where the credential rows render).
# Its own set because an app's page can land before its mail rows can (web's
# page lives in a wasm chunk that does not hold the mail machine — web is still
# out of the set for that reason). It disappears when web joins.
MAIL_ROWS_ON_CONNECTED_APPS = frozenset(
    {"tui", "android", "linux", "macos", "ios", "windows"}
)


def mail_rows_on_connected_apps(driver: PlatformDriver) -> bool:
    return app_name(driver) in MAIL_ROWS_ON_CONNECTED_APPS


class ConnectedAppsActions:
    """Drive the Settings → Connected apps sub-page (docs/goal/ui/connected-apps.md;
    tests/e2e-unified/ui.yaml `connected-apps`).

    Four regions: the **Requests** tray (`connected-apps-request-card`,
    indexed, rendered only while a request is live — no empty row), **Connect an
    app** (`connected-apps-connect-code` + `-connect-submit`, the typed-code
    start), **the roster** (`connected-apps-item`, indexed, Revoke with an
    inline confirm; `connected-apps-empty` once the read returned empty; a mail
    app-password row adds its login, kind and secret leaves), and **Blocked
    apps** (`connected-apps-blocked-item`, indexed, rendered only while
    something is blocked).

    Rows are nest state read on every visit, and a quiet push raises no event,
    so the waits below re-navigate while they poll: a visit IS the re-read. A
    fresh visit paints neither rows nor the empty state until its own read has
    returned, so `visit()` (navigate, then `wait_loaded`) is the read that
    cannot answer from a previous visit — use it before any read that is not
    itself a wait.
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "connected-apps"}]},
        })

    def is_page_visible(self, timeout: float = 10.0) -> bool:
        try:
            self.driver.wait_for("connected-apps-connect-code", timeout=timeout)
        except TimeoutError:
            return False
        return True

    def wait_loaded(self, timeout: float = RPC_ROUNDTRIP_S) -> bool:
        """Wait for THIS visit's read to return: the roster then paints its
        rows or its empty state, and before that neither (the three-state
        list). Deadline-polled (convention 14)."""
        deadline = time.monotonic() + timeout
        while True:
            if self.item_count() > 0 or self.driver.count("connected-apps-empty") > 0:
                return True
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.1)

    def visit(self, timeout: float = RPC_ROUNDTRIP_S) -> bool:
        """Open the page and wait for its read — whether it returned in time."""
        self.navigate()
        return self.wait_loaded(timeout)

    # ── Requests ──────────────────────────────────────────────────────────
    def request_count(self) -> int:
        return self.driver.count("connected-apps-request-card")

    def request_text(self, index: int) -> str:
        return self.driver.get_text("connected-apps-request-card", index=index) or ""

    def request_code(self, index: int) -> str:
        """The binding code VALUE — the element's `code` attr, never its prose."""
        return (
            self.driver.get_attr(
                "connected-apps-request-code",
                "code",
                scope=f"connected-apps-request-card[{index}]",
            )
            or ""
        )

    def request_index_for_code(self, code: str) -> int | None:
        """Which card shows `code`. Tests find their OWN request this way: every
        unhinted request on the nest is listed to every account, so a positional
        assumption could answer somebody else's sign-in."""
        for i in range(self.request_count()):
            if self.request_code(i) == code:
                return i
        return None

    def request_index_for_client(self, client_id: str) -> int | None:
        for i in range(self.request_count()):
            if client_id in self.request_text(i):
                return i
        return None

    def _poll(self, probe, timeout: float, *, renavigate: bool):
        deadline = time.monotonic() + timeout
        while True:
            found = probe()
            if found is not None or time.monotonic() >= deadline:
                return found
            if renavigate:
                # A revisit re-reads, and paints nothing until that read is
                # back — so the next probe waits for it rather than racing it.
                self.visit(max(deadline - time.monotonic(), 0.5))
            time.sleep(0.3)

    def wait_for_request_code(
        self, code: str, timeout: float = RPC_ROUNDTRIP_S, renavigate: bool = True
    ) -> int | None:
        """Wait for a card showing `code`; its index or None. Deadline-polled
        (convention 14), re-reading by revisiting the page."""
        return self._poll(
            lambda: self.request_index_for_code(code), timeout, renavigate=renavigate
        )

    def wait_for_request_from(
        self, client_id: str, timeout: float = RPC_ROUNDTRIP_S
    ) -> int | None:
        return self._poll(
            lambda: self.request_index_for_client(client_id), timeout, renavigate=True
        )

    def approve_request(self, index: int) -> None:
        self.driver.click(
            "connected-apps-request-approve", scope=f"connected-apps-request-card[{index}]"
        )

    def decline_request(self, index: int) -> None:
        self.driver.click(
            "connected-apps-request-decline", scope=f"connected-apps-request-card[{index}]"
        )

    def block_request(self, index: int) -> None:
        """*Never show requests from this app*."""
        self.driver.click(
            "connected-apps-request-block", scope=f"connected-apps-request-card[{index}]"
        )

    # ── Connect an app ────────────────────────────────────────────────────
    def connect_with_code(self, code: str) -> None:
        self.driver.wait_for("connected-apps-connect-code", timeout=10.0)
        self.driver.clear_and_type("connected-apps-connect-code", code)
        self.driver.click("connected-apps-connect-submit")

    # ── The same-device handoff ───────────────────────────────────────────
    def open_handoff_route(self, request_uri: str) -> None:
        """Hand the app a `fauna://consent/<request_uri>` route, as a device
        app's link would — through the `open_route` automation command, which
        feeds the URI to the same door the app's own route intake takes
        (`apps/tui.md` § System integration → *In-app routes*). The command
        acks once the route's page work has landed, so the card is painted
        on return. Built on tui, macos, ios and windows so far (the Apple pair
        take it through `onOpenURL`'s one FaunaKit door, `apps/ios.md` § App
        Entry → *In-app routes*; windows through `App.ApplyRoute`, the argv
        leg's door); the other apps' intake rides the trickle-down row.
        **web is a declared absence** (`apps/web.md` § Implementation
        status today): a browser tab is launched by no OS link, and a device
        app whose open finds no handler takes the nest's browser door instead
        (`behavior/authorization-server.md` § Consent → *How the same-device
        handoff is built*) — so there is no route to feed and no command to
        add; the test's `web` marker is this guard's collection-time shadow
        (convention 7's rider)."""
        from helpers.app_surface import declared_absence

        if self.driver.is_web():
            declared_absence(
                self.driver,
                capability="fauna://consent route intake (the same-device handoff's app half)",
                doc="apps/web.md § Implementation status today",
            )
        self.driver.call_command("open_route", {"uri": f"fauna://consent/{request_uri}"})

    # ── The roster ────────────────────────────────────────────────────────
    def item_count(self) -> int:
        return self.driver.count("connected-apps-item")

    def item_text(self, index: int) -> str:
        return self.driver.get_text("connected-apps-item", index=index) or ""

    def item_index_containing(self, needle: str) -> int | None:
        for i in range(self.item_count()):
            if needle in self.item_text(i):
                return i
        return None

    def wait_for_item_containing(
        self, needle: str, timeout: float = RPC_ROUNDTRIP_S
    ) -> int | None:
        """Wait for a roster row whose text contains `needle`, re-reading by
        revisiting the page (a new row appears when the client redeems its
        grant — nothing pushes it)."""
        return self._poll(
            lambda: self.item_index_containing(needle), timeout, renavigate=True
        )

    def wait_for_no_item_containing(
        self, needle: str, timeout: float = RPC_ROUNDTRIP_S
    ) -> bool:
        gone = self._poll(
            lambda: True if self.item_index_containing(needle) is None else None,
            timeout,
            renavigate=True,
        )
        return bool(gone)

    def revoke_item(self, index: int) -> None:
        """Revoke → the inline confirm → confirm."""
        scope = f"connected-apps-item[{index}]"
        self.driver.click("connected-apps-item-revoke", scope=scope)
        self.driver.wait_for("connected-apps-item-revoke-confirm", timeout=10.0)
        self.driver.click("connected-apps-item-revoke-confirm", scope=scope)

    # ── Mail app-password rows ────────────────────────────────────────────
    # The roster is mixed-class and oldest first: an ATProto app-password
    # session shares the mail rows' class badge. A mail row is picked out by
    # the leaves only it carries, never by its badge or a bare roster index.
    # `n` below counts mail rows only, in roster order.
    MAIL_LOGIN = "connected-apps-item-username"

    def mail_row_count(self) -> int:
        return self.driver.count(self.MAIL_LOGIN)

    def mail_row_items(self) -> list[int]:
        """The roster index of each mail app-password row, in order."""
        return [
            i
            for i in range(self.item_count())
            if self.driver.count(self.MAIL_LOGIN, scope=f"connected-apps-item[{i}]") > 0
        ]

    def mail_item(self, n: int) -> int:
        """The roster index of the `n`-th mail row."""
        items = self.mail_row_items()
        if n >= len(items):
            raise LookupError(f"mail app-password row {n} of {len(items)}")
        return items[n]

    def wait_for_mail_rows(self, accept, timeout: float = RPC_ROUNDTRIP_S) -> bool:
        """Wait until `accept(mail_row_count())` holds, re-reading by
        revisiting the page (a new password appears when its mint lands —
        nothing pushes it)."""
        return bool(
            self._poll(
                lambda: True if accept(self.mail_row_count()) else None,
                timeout,
                renavigate=True,
            )
        )

    def mail_login(self, n: int) -> str:
        return (self.driver.get_text(self.MAIL_LOGIN, n) or "").strip()

    def mail_kind(self, n: int) -> str:
        return (self.driver.get_text("connected-apps-item-type", n) or "").strip()

    def mail_name(self, n: int) -> str:
        """The row's name: the item's first line, less its class badge."""
        head = (self.item_text(self.mail_item(n)).splitlines() or [""])[0]
        badge = f" · {S.connected_apps.class_app_password}"
        return head[: -len(badge)] if head.endswith(badge) else head

    def mail_created(self, n: int) -> str:
        """When the password was made, as the row says it (`YYYY-MM-DD HH:MM`)."""
        prefix = S.connected_apps.created(time="")
        for line in self.item_text(self.mail_item(n)).splitlines():
            for fact in line.split(" · "):
                if fact.startswith(prefix):
                    return fact[len(prefix):].strip()
        return ""

    def mail_revoked_count(self) -> int:
        """How many mail rows read *access revoked* — the identity-succession
        burn's state, carried as the item's `revoked` attr beside its words."""
        return sum(
            1
            for i in self.mail_row_items()
            if self.driver.get_attr("connected-apps-item", "revoked", i) == "true"
        )

    def reveal_mail_secret(self, n: int, timeout: float = 8.0) -> str:
        """Click reveal on the `n`-th mail row and return the secret once it
        shows. The leaf is present and EMPTY until the on-demand read lands."""
        self.driver.click("connected-apps-item-reveal-secret", n)
        deadline = time.monotonic() + timeout
        while True:
            try:
                secret = (self.driver.get_text("connected-apps-item-secret", n) or "").strip()
            except LookupError:
                secret = ""
            if secret or time.monotonic() >= deadline:
                return secret
            time.sleep(0.25)

    def revoke_mail_row(self, n: int) -> None:
        self.revoke_item(self.mail_item(n))

    # ── Blocked apps ──────────────────────────────────────────────────────
    def blocked_count(self) -> int:
        return self.driver.count("connected-apps-blocked-item")

    def blocked_index_for_client(self, client_id: str) -> int | None:
        for i in range(self.blocked_count()):
            if client_id in (self.driver.get_text("connected-apps-blocked-item", index=i) or ""):
                return i
        return None

    def wait_for_blocked(self, client_id: str, timeout: float = RPC_ROUNDTRIP_S) -> int | None:
        return self._poll(
            lambda: self.blocked_index_for_client(client_id), timeout, renavigate=True
        )

    def wait_for_not_blocked(self, client_id: str, timeout: float = RPC_ROUNDTRIP_S) -> bool:
        return bool(
            self._poll(
                lambda: True if self.blocked_index_for_client(client_id) is None else None,
                timeout,
                renavigate=True,
            )
        )

    def unblock(self, index: int) -> None:
        self.driver.click(
            "connected-apps-blocked-item-unblock",
            scope=f"connected-apps-blocked-item[{index}]",
        )

    def current_error_text(self) -> str:
        """Read `error-message` RIGHT NOW — no polling (the bluesky helper's
        rule: a negative assert that polls is a settle-sleep). An app paints the
        element only while an error is set, so absent reads as no error."""
        if not self.driver.is_visible("error-message"):
            return ""
        return (self.driver.get_text("error-message") or "").strip()
