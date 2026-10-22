from __future__ import annotations

import time
from typing import TYPE_CHECKING

from helpers.budgets import RPC_ROUNDTRIP_S
from i18n.strings import S

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


# The Connected apps page is built on all 7 apps, so the rows it lifted (the
# AT Protocol consent card + connected-app rows, the Nostr bunker rows, and Mail
# & Calendar's app passwords) render HERE and nowhere else
# (docs/goal/ui/connected-apps.md § Implementation status today;
# docs/goal/ui/mail-settings.md § Where the credential rows render).


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

    # The install section — only on an admin's hosted-plugin install card
    # (third-party.md § The install-approval leg). `leaf` is the id's suffix
    # after `connected-apps-request-install`: "" (the heading), "form",
    # "publisher", "digest", "kinds", "hosts" or "settings".
    def request_install_text(self, index: int, leaf: str = "") -> str:
        element = "connected-apps-request-install" + (f"-{leaf}" if leaf else "")
        scope = f"connected-apps-request-card[{index}]"
        if self.driver.count(element, scope=scope) == 0:
            return ""
        return self.driver.get_text(element, scope=scope) or ""

    def request_install_form(self, index: int) -> str:
        """The execution form VALUE — the badge's `form` attr."""
        return (
            self.driver.get_attr(
                "connected-apps-request-install-form",
                "form",
                scope=f"connected-apps-request-card[{index}]",
            )
            or ""
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
        on return. Built on tui, macos, ios, windows, android and linux (the
        Apple pair take it through `onOpenURL`'s one FaunaKit door,
        `apps/ios.md` § App Entry → *In-app routes*; windows through
        `App.ApplyRoute`, the argv leg's door; android through
        `ConsentHandoff.offer`, the door its `fauna://` intent filter feeds,
        `apps/android.md` § Navigation → *In-app routes*; linux through
        `routes::offer`, the door GApplication's `open` leg feeds,
        `apps/linux.md` § System integration → *In-app routes*).
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

    def item_status(self, index: int) -> str:
        """Whether the row can act — `connected`, `not_connected`, or `paused`
        (the account's external-apps switch is OFF) — the item's `status`
        attr beside its words. Built on tui; the other apps' render rides the
        trickle-down lift."""
        return self.driver.get_attr("connected-apps-item", "status", index) or ""

    def wait_for_item_status(
        self, needle: str, status: str, timeout: float = RPC_ROUNDTRIP_S
    ) -> int | None:
        """Wait for the row whose text contains `needle` to read `status`,
        re-reading by revisiting the page (a switch flip on another page
        pushes nothing here); its index or None."""

        def probe():
            index = self.item_index_containing(needle)
            if index is not None and self.item_status(index) == status:
                return index
            return None

        return self._poll(probe, timeout, renavigate=True)

    def item_oracle_ops(self, index: int) -> str | None:
        """The operations the oracle ran for a served principal in the current
        window — the item's `oracle_ops` attr beside its words; `None` on a row
        the oracle never served. Built on tui; the other apps' render rides the
        trickle-down lift."""
        return self.driver.get_attr("connected-apps-item", "oracle_ops", index)

    def wait_for_oracle_item_containing(
        self, needle: str, timeout: float = RPC_ROUNDTRIP_S
    ) -> int | None:
        """Wait for a row whose text contains `needle` and which carries the
        oracle summary, re-reading by revisiting the page (an operation the
        oracle runs pushes nothing here); its index or None."""

        def probe():
            index = self.item_index_containing(needle)
            if index is not None and self.item_oracle_ops(index) is not None:
                return index
            return None

        return self._poll(probe, timeout, renavigate=True)

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

    def has_settings_link(self, index: int) -> bool:
        """Whether the row links to its settings card — a hosted plugin's row
        only (`connected-apps-item-settings-link`, connected-apps.md § Layout &
        flow, region 3). Built on tui; the other apps ride the trickle-down."""
        return (
            self.driver.count(
                "connected-apps-item-settings-link", scope=f"connected-apps-item[{index}]"
            )
            == 1
        )

    def open_settings_link(self, index: int) -> None:
        """Follow a plugin row's link to its settings card on the Bridges page."""
        self.driver.click(
            "connected-apps-item-settings-link", scope=f"connected-apps-item[{index}]"
        )

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

    def revoked_item_count(self) -> int:
        """How many roster rows of ANY class read *access revoked* — the
        identity-succession burn's state (a mail app password, a Nostr signer
        client), the item's `revoked` attr beside its words. Built on tui; the
        other apps' signer paint rides the trickle-down."""
        return sum(
            1
            for i in range(self.item_count())
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

    # ── The account's external-apps switch (`atproto-external-apps-enable`) ──
    # On this page at every integration level (ui.yaml `connected-apps`;
    # docs/goal/ui/connected-apps.md § Layout & flow). Its on/off rides the
    # `state` attr, the same idiom as `mail-settings-enabled-toggle`.
    EXTERNAL_APPS_TOGGLE_ID = "atproto-external-apps-enable"

    def external_apps_state(self) -> str:
        return self.driver.get_attr(self.EXTERNAL_APPS_TOGGLE_ID, "state") or ""

    def wait_for_external_apps_state(self, expected: str, timeout: float = 10.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.external_apps_state() == expected:
                return True
            time.sleep(0.3)
        return self.external_apps_state() == expected

    def set_external_apps_enabled(self, enabled: bool) -> None:
        """Drive the switch to `enabled`, idempotently."""
        self.driver.wait_for(self.EXTERNAL_APPS_TOGGLE_ID, timeout=10.0)
        want = "on" if enabled else "off"
        if self.external_apps_state() != want:
            self.driver.click(self.EXTERNAL_APPS_TOGGLE_ID)
