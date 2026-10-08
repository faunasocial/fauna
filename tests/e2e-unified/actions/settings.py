from __future__ import annotations

import time
from typing import TYPE_CHECKING

from i18n.strings import S

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


#: How long the Recovery Kit section may take to finish its **chain read** before
#: its ceremonies are actionable — the async round trip behind `allowsCreate` and
#: friends (`recovery_kit_status` reads the registration chain, never a local
#: flag). Generous on purpose and paid only on red: convention 14 wants a ceiling
#: far above any non-pathological delay, not a settle-sleep sized to the happy
#: path. Sized like the section's other nest-backed budgets, not like a render.
RECOVERY_KIT_READY_WAIT_S = 30.0


class SettingsActions:
    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        """Navigate to the Settings shell.

        On the desktop sidebar-swap shells (linux/web/windows/macos) this
        lands on the default (Status) sub-page, which carries the live
        identity/quota IDs (`account-actor-id`, `quota-*`,
        `status-*-copy-btn`). iOS's idiomatic settings nav is the ONE
        exception: a plain navigate lands on the root page *list*, not any
        sub-page (`SettingsPage.swift`'s `init(navId:)` doc comment) — a test
        that needs identity/quota data must call `_navigate_subpage("status")`
        or `_navigate_subpage("account")` explicitly (see
        `test_actor_id_visible`/`test_quota_section`)."""
        self.driver.navigate_to("settings")

    def _navigate_subpage(self, page_id: str) -> None:
        """Navigate to a specific Settings sub-page.

        Every app is a genuinely sub-paged shell — there is no remaining
        single-scroll client whose sub-id is ignored. The desktop
        sidebar-swap shells (linux, windows's `SettingsShellPage`, and macOS
        since its `SettingsShellView` migration off the single-scroll
        `PreferencesView` stack) and web's `routes/settings/[[subpage]]`
        route each show one sub-page at a time, with its own content
        (`account` and `status` are NOT interchangeable; see
        `test_actor_id_visible`). The explicit id `"status"` and the bare
        `{"view":"settings"}` (no id) both resolve to the same default
        landing page on every app — the same cross-app-safe
        two-element nav the admin actions use
        (`{"view":"settings"},{"view":"settings","id":"<page>"}`).
        **Corrected 2026-07-30:** this docstring used to claim web/windows
        were single-scroll clients that ignore the sub-id; both are fully
        sub-paged. Web's rail mapped Status to an empty-string id instead of
        the uniform `"status"` id every other app accepts, which silently
        broke this call there — see `test_actor_id_visible[web]`."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": page_id}]},
        })

    # ── About: the version, and the newer-version check ──────────────────────
    # (`docs/features/app-version-and-updates.md`; installers/README.md § Knowing
    # a newer version is out). Where the block sits is per-app: the Settings
    # root on tui, the General page on linux/windows/macOS (macOS's app-menu
    # item is a second door onto the same check) — the ids are the same
    # everywhere, and `navigate_to_about()` lands where they render.

    def navigate_to_about(self) -> None:
        """Land where the About block renders: the Settings root on tui, the
        General sub-page on the desktop sidebar shells."""
        if self.driver.is_tui():
            self.navigate()
        else:
            self._navigate_subpage("general")

    def app_version(self) -> str:
        """The version the app says it is running (`settings-app-version`)."""
        self.driver.wait_for("settings-app-version")
        return (self.driver.get_text("settings-app-version") or "").strip()

    def check_for_updates(self, timeout: float = 30.0) -> None:
        """Press the check and wait for it to resolve.

        The button carries the check's state in its own label — "Checking…"
        while in flight — so resolution is observed as the button leaving that
        label (latency-independent, convention 14), never as a delay."""
        from helpers.waiting import wait_until

        self.driver.wait_for("settings-check-updates-button")
        self.driver.click("settings-check-updates-button")

        def resolved():
            label = (self.driver.get_text("settings-check-updates-button") or "").strip()
            return label if label and label != S.common.checking else None

        wait_until(
            resolved,
            timeout,
            diagnose=lambda: (
                f"the newer-version check did not resolve — the button still reads "
                f"{self.driver.get_text('settings-check-updates-button')!r}"
            ),
        )

    def update_notice(self, timeout: float = 10.0) -> str:
        """The `update-available-notice` text, waited for."""
        self.driver.wait_for("update-available-notice", timeout=timeout)
        return (self.driver.get_text("update-available-notice") or "").strip()

    def change_handle(self, new_handle: str) -> None:
        """Change the user's handle (Account sub-page)."""
        self._navigate_subpage("account")
        self.driver.wait_for("new-handle")
        self.driver.clear_and_type("new-handle", new_handle)
        self.driver.click("change-handle")
        time.sleep(2)

    def sign_out(self) -> None:
        """Sign out from the Account sub-page: clears local credentials + state
        and returns the app to onboarding (identity_choice).

        Mirrors the destructive-confirm-then-onboarding shape of
        ``AdminActions.factory_reset_via_ui``: click ``sign-out-button``, then
        the inline ``sign-out-confirm-button``. The client wipes the stored
        secret and re-roots onboarding at identity_choice, so the user must
        re-create/import an identity (their secret key) to sign back in."""
        self.press_sign_out()
        # Session torn down; onboarding re-rooted at identity_choice.
        self.driver.wait_for("create-identity-button", timeout=30.0)

    def press_sign_out(self) -> None:
        """Press sign-out and its confirm, asserting nothing about the outcome —
        the gesture half of ``sign_out`` for a test whose sign-out may be
        REFUSED (``account-scoping.md`` § Concurrent instances → *An erase
        refuses while a sibling serves the account*)."""
        self._navigate_subpage("account")
        self.driver.wait_for("sign-out-button", timeout=10.0)
        self.driver.click("sign-out-button")
        self.driver.wait_for("sign-out-confirm-button", timeout=10.0)
        self.driver.click("sign-out-confirm-button")

    def type_delete_confirm(self, text: str) -> None:
        """Type into the account-deletion type-to-confirm buffer
        (``settings-delete-confirm-field``, Account sub-page)."""
        self._navigate_subpage("account")
        self.driver.clear_and_type("settings-delete-confirm-field", text)

    def is_delete_account_enabled(self) -> bool:
        """Whether ``settings-delete-account-button`` is currently clickable —
        gated on the confirm field reading exactly "DELETE"."""
        return self.driver.is_enabled("settings-delete-account-button")

    def confirm_delete_account(self) -> None:
        """Click the (now-enabled) delete-account button — queues the 14-day
        pending ``fauna.account.delete``; does not delete immediately."""
        self.driver.click("settings-delete-account-button")

    def open_identity_export(self) -> None:
        """Navigate to the Account sub-page and wait for the identity-export section.

        The section always renders its description + toggle; the warning and the QR
        appear only once the user presses show (settings.md § Identity export)."""
        self._navigate_subpage("account")
        self.driver.wait_for("identity-export-show-qr-button", timeout=10.0)

    def export_my_data(self) -> bytes:
        """Drive the account page's `settings-export-data-button` and return the
        downloaded archive's bytes, whichever way the platform exposes them (the
        platform branch lives here in the action layer): web captures the real
        browser download (Playwright wraps the click server-side, so it can't be
        composed from a plain click); native apps save dialog-less as
        `fauna-export.zip` into `driver.download_dir()` — tui always saves to its
        downloads dir, linux bypasses its save dialog under e2e via the shared
        `FAUNA_E2E_DOWNLOAD_DIR` seam, apple's `AccountSettingsView.exportAndPresent`
        and windows' `SettingsViewModel.ExportAccountDataAsync` take the same seam
        via `SnapshotFileSaver`/`ISnapshotFileSaver` — and the file is read back
        from there. Built on all 7 apps."""
        self._navigate_subpage("account")
        self.driver.wait_for("settings-export-data-button")
        if self.driver.is_web():
            return self.driver.download_via_click("settings-export-data-button")
        self.driver.click("settings-export-data-button")
        from actions.backups import BackupsActions

        return BackupsActions(self.driver).wait_for_downloaded_file("fauna-export.zip")

    def toggle_identity_qr(self) -> None:
        """Press the identity-export show/hide toggle once.

        One button, not two: its label flips between ``show_qr`` and ``hide_qr``."""
        self.driver.click("identity-export-show-qr-button")

    def identity_qr_shown(self) -> bool:
        """Whether the identity QR is currently revealed.

        The QR and its warning are the two elements gated on the toggle, so this is
        the load-bearing read for the reveal contract.

        Read as tree membership (``not is_absent``), never a bare ``is_visible``: the
        QR is the SECRET, and the contract's negative half — *it stays hidden until
        the user presses show* — is asserted through this helper. On windows
        ``is_visible`` answers ``!IsOffscreen``, so a QR that a defect painted below
        the fold of the Account page's ScrollViewer would read "not shown" and let
        the very defect this asserts against pass. Everywhere else ``is_absent`` IS
        ``not is_visible``, so nothing changes there. (A helper-mediated negative
        read is invisible to ``check_negative_visibility_ratchet.py``, which sees
        only a literal ``assert not …is_visible(`` — the reason this reads through
        the primitive itself.)"""
        return not self.driver.is_absent("identity-export-qr")

    def identity_export_toggle_label(self) -> str:
        """The toggle's current label — ``show_qr`` when collapsed, ``hide_qr`` when
        revealed. Reading it proves the label actually flips rather than the section
        merely growing a second button."""
        return self.driver.get_text("identity-export-show-qr-button")

    def open_icloud_backup(self) -> None:
        """Navigate to the Account sub-page and wait for the iCloud-backup toggle.

        Apple-only (macos + ios); the toggle sits beside Identity Export
        (apps/ios.md § Credential Storage)."""
        self._navigate_subpage("account")
        self.driver.wait_for("settings-icloud-backup-toggle", timeout=10.0)

    def icloud_backup_state(self) -> str:
        """The iCloud-backup toggle's current value — "on" when the identity is backed
        up to iCloud Keychain, "off" (the default) when it is device-bound."""
        return self.driver.get_attr("settings-icloud-backup-toggle", "state") or ""

    def set_icloud_backup(self, on: bool) -> None:
        """Drive the iCloud-backup toggle to on/off idempotently."""
        self.driver.wait_for("settings-icloud-backup-toggle", timeout=10.0)
        want = "on" if on else "off"
        if self.icloud_backup_state() != want:
            self.driver.click("settings-icloud-backup-toggle")

    def open_privacy(self) -> None:
        """Navigate to the Privacy sub-page and wait for it to render.

        The read-only counterpart to `set_inbox_mode` / `save_spam_preferences`:
        every other Privacy action navigates as a side effect of mutating
        something, so a test that only wants to *read* the page had no way to
        get there without also changing it — which is exactly what a test
        asserting "the page shows the account's stored value" must not do.

        Anchors on `spam-preferences` rather than an `inbox-mode-*` radio: the
        radios are the thing under test, and on a client that renders only the
        SELECTED radio's test id (web) an unknown mode legitimately paints none
        of them, so waiting on one would turn a readable assertion into a
        timeout.
        """
        from helpers.budgets import UI_SETTLE_S

        self._navigate_subpage("privacy")
        self.driver.wait_for("spam-preferences", timeout=UI_SETTLE_S)

    #: Apps whose Privacy nav edge IS the mode read: the nav command does not
    #: complete until `inbox_mode_get` has answered, so there is no moment at
    #: which the page is reachable with the read unresolved. tui awaits
    #: `Op::FetchPrivacy` inside `apply_nav` before the command acks — a
    #: deliberate choice (a fire-and-forget spawn let a stale nav-edge refetch
    #: clobber a fresher mutation), and it means holding the read makes the NAV
    #: time out rather than exposing a pending page: measured 2026-08-27, the
    #: nav acked ~5s later carrying `inbox_mode_get: The nest took too long to
    #: respond`, with the request already abandoned.
    #:
    #: Default is "has a window": an app this set has never heard of gets
    #: TESTED, never excused. A capability table that defaults to *absent* is
    #: the shape `e2e-conventions.md` point 7 warns about — it answers "not
    #: implemented" for apps it never heard of and hides real gaps.
    _PRIVACY_NAV_AWAITS_ITS_MODE_READ = frozenset({"tui"})

    def privacy_nav_awaits_its_mode_read(self) -> bool:
        """Whether this app's Privacy nav completes only once the mode read has.

        See :attr:`_PRIVACY_NAV_AWAITS_ITS_MODE_READ`. Such an app satisfies
        `settings.md` § Privacy sub-page item 7 structurally — it cannot show a
        mode it has not read, because it does not show the page — so there is no
        pending page to assert on; the nav itself is the window, witnessed by
        `test_settings.py::test_privacy_nav_names_no_mode_while_the_mode_read_is_held`.
        """
        from helpers.app_surface import app_name
        return app_name(self.driver) in self._PRIVACY_NAV_AWAITS_ITS_MODE_READ

    def request_privacy(self) -> None:
        """Ask for the Privacy sub-page and return at once — no render wait.

        The counterpart to :meth:`open_privacy` for the one window where waiting
        is wrong: while ``inbox_mode_get`` is still unanswered, the apps
        legitimately differ on whether the page is on screen at all. linux and
        web build it immediately and repaint when the mode lands; tui awaits
        ``Op::FetchPrivacy`` on the nav edge, so nothing renders until the reply
        arrives. :meth:`open_privacy` anchors on ``spam-preferences`` and would
        therefore hang on tui for as long as the read is held — turning a
        deliberate pending-state assertion into a timeout.

        Both shapes honour `settings.md` § Privacy sub-page (neither shows a mode
        the user did not choose), and `get_inbox_mode` reports ``""`` under both,
        so a test that navigates with this and then asserts on the *mode* is
        asserting the rule rather than one app's rendering strategy.

        ``wait_ready=False`` for the same reason: the bridge drivers' `set_state`
        normally waits for the app to report itself settled, and on tui the nav
        edge awaits ``Op::FetchPrivacy`` — the very read a caller of this method
        is holding — so waiting would block on the state the caller means to
        observe. The command is still acked; the caller waits on its own
        observable next.
        """
        self.driver.set_state(
            {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "privacy"}]}},
            wait_ready=False,
        )

    def set_inbox_mode(self, mode: str) -> None:
        """Set inbox mode. mode is one of: open, allow_knock, contacts_only, closed."""
        self._navigate_subpage("privacy")
        testid = f"inbox-mode-{mode}"
        self.driver.click(testid)
        # Wait for the state to propagate through the bridge.  AT-SPI clicks
        # update the GTK widget synchronously, but the state JSON must be
        # serialised and pushed to the bridge (~1-2 poll cycles).
        if hasattr(self.driver, "get_state"):
            deadline = time.monotonic() + 5.0
            last_settings = None
            while time.monotonic() < deadline:
                last_settings = self.driver.get_state("settings")
                if isinstance(last_settings, dict) and last_settings.get("inbox_mode") == mode:
                    return
                time.sleep(0.3)
            raise AssertionError(
                f"inbox_mode did not settle on {mode!r} within 5.0s (last "
                f"settings state: {last_settings!r}); {self.driver.diagnose(testid)}"
            )
        time.sleep(0.5)

    def get_inbox_mode(self) -> str:
        """The mode the page is currently showing, or ``""`` for *unknown*.

        Prefers the state protocol (``settings.inbox_mode``) when available,
        because AT-SPI ``is_visible`` returns True for all toggle buttons
        in a linked group regardless of which one is active.

        **``""`` is an answer, not a miss** — it is the mode the app reports
        while ``inbox_mode_get`` has not answered, which `settings.md` § Privacy
        sub-page makes a required state ("Until ``inbox_mode_get`` has answered,
        the mode is *unknown*"): all four radios paint unmarked and the page says
        why. So when the state protocol carries the key at all, its value is
        returned verbatim — empty string included. This used to fall through to
        the visibility scan on an empty value, and the scan then reported
        ``"open"``: on linux and tui all four radios are present-and-unmarked
        during exactly that window, so the first one always matched. The action
        could therefore never express "unknown" on the apps that render it, and
        reported a mode the user had not chosen — the very lie the rule exists
        to forbid, told by the instrument meant to catch it.

        The visibility scan stays for apps with no ``settings.inbox_mode`` key
        to read; there it remains the only signal available.
        """
        # Try reading from the test agent state first (works on all
        # bridge-backed drivers: Linux, Windows, iOS, macOS).
        if hasattr(self.driver, "get_state"):
            settings = self.driver.get_state("settings")
            if isinstance(settings, dict) and "inbox_mode" in settings:
                return settings["inbox_mode"] or ""

        # Fallback: check which element is visible.
        for mode in ("open", "allow_knock", "contacts_only", "closed"):
            testid = f"inbox-mode-{mode}"
            if self.driver.is_visible(testid):
                return mode
        return ""

    def save_spam_preferences(self, spam_threshold: str = "",
                               phishing_threshold: str = "") -> None:
        """Adjust and save spam preferences."""
        self._navigate_subpage("privacy")
        self.driver.wait_for("spam-preferences")
        # Sliders use clear_and_type for range inputs
        if spam_threshold:
            self.driver.clear_and_type("spam-threshold", spam_threshold)
        if phishing_threshold:
            self.driver.clear_and_type("phishing-threshold", phishing_threshold)
        self.driver.click("save-spam-prefs")
        time.sleep(1)

    def create_email_filter(self, name: str, rule_type: str, rule_value: str,
                            action: str, timeout: float = 15, *,
                            forward_address: str | None = None,
                            keep_local_copy: bool = True) -> None:
        """Create an email filter, returning once the row is actually there.

        Polls for the new row rather than sleeping a fixed second (convention
        14). The old `time.sleep(1)` was a wall-clock bet on the create round
        trip, and it lost: `test_email_filter_edit[windows]` failed as "create
        didn't land; have []" on a loaded box — an empty list, i.e. the page had
        not rendered ANY row yet, not a create that went missing. The sibling
        `delete_filter` below already polls for exactly this reason (its
        docstring records the same failure on iOS) and says in as many words
        that the create leg's fixed sleep is the weaker half; this makes the
        pair symmetric.

        A green run pays only the real round-trip time, so the budget is
        generous: only a genuinely lost create spends it.

        A `Forward` action takes `forward_address` (typed into
        `filter-forward-address`) and `keep_local_copy` (the
        `filter-keep-local-copy` checkbox, checked by default; `False` is the
        redirect copy mode).
        """
        self._navigate_subpage("privacy")
        before = self.driver.count("filter-item")
        self.driver.click("add-filter-btn")
        self.driver.wait_for("filter-name-input")
        self.driver.clear_and_type("filter-name-input", name)
        # filter-rule-type / filter-action-select are <select> elements — use
        # the uniform select() driver method (text inputs use clear_and_type).
        self.driver.select("filter-rule-type", rule_type)
        self.driver.clear_and_type("filter-rule-value", rule_value)
        self.driver.select("filter-action-select", action)
        if forward_address is not None:
            self.driver.wait_for("filter-forward-address")
            self.driver.clear_and_type("filter-forward-address", forward_address)
            self.set_filter_keep_local_copy(keep_local_copy)
        self.driver.click("create-filter")
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self._navigate_subpage("privacy")
            if self.driver.count("filter-item") > before:
                return
            time.sleep(0.5)  # sleep-ok: poll cadence inside a deadline poll
        raise TimeoutError(
            f"filter {name!r} did not appear within {timeout}s of create "
            f"(row count still {self.driver.count('filter-item')}, was {before})"
        )

    def filter_count(self) -> int:
        self._navigate_subpage("privacy")
        return self.driver.count("filter-item")

    def filter_names(self) -> list[str]:
        self._navigate_subpage("privacy")
        count = self.driver.count("filter-name")
        return [self.driver.get_text("filter-name", index=i) for i in range(count)]

    def delete_filter(self, index: int = 0, timeout: float = 10) -> None:
        """Delete the nth filter.

        Polls until the row count actually drops rather than a fixed sleep —
        found via `test_email_filter_crud[ios]` failing reproducibly (2/2
        solo) at `filter_count() == initial + 1` right after delete: the
        create leg's fixed `sleep(1)` above is enough on iOS, but the delete
        round-trip evidently isn't."""
        self._navigate_subpage("privacy")
        before = self.driver.count("filter-item")
        self.driver.click("filter-delete", index=index)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self._navigate_subpage("privacy")
            if self.driver.count("filter-item") < before:
                return
            time.sleep(0.5)
        raise TimeoutError(
            f"filter-item count did not drop below {before} within {timeout}s "
            f"after deleting index {index}"
        )

    def filter_action(self, index: int = 0) -> str:
        """Get the action text for a filter at the given index."""
        self._navigate_subpage("privacy")
        return self.driver.get_text("filter-action", index=index)

    def filter_edit_visible(self, index: int = 0) -> bool:
        """Whether the nth filter row offers an edit affordance (gated
        filter_is_editable — a filter only a raw API call could have
        produced, outside the dialog's dropdown-covered subset, has none).

        Explicit targeted scroll first: the email-filters section sits at the
        bottom of a page taller than the viewport on single-scroll clients
        (windows' SettingsPrivacyPage), and _navigate_subpage's set_state
        re-navigate resets scroll to top on every call — unlike count()/
        get_text()/click(), is_visible() checks on-screen position, so a row
        below the fold reads as "not visible" even though it rendered
        correctly (mirrors feed.py's scroll_post_into_view rationale).
        `is_visible_scrolled` degrades to a plain `is_visible` on drivers with
        no scroll-into-view support (apple: `_supports_scroll_into_view =
        False`) instead of raising `NotImplementedError` — found via
        `test_email_filter_edit[macos]` failing reproducibly at this exact
        line, 3/3."""
        self._navigate_subpage("privacy")
        return self.driver.is_visible_scrolled(
            "filter-edit", scope=f"filter-item[{index}]"
        )

    def filter_keep_local_copy(self) -> bool:
        """The open filter form's `filter-keep-local-copy` checkbox state (a
        Forward rule's copy mode: checked = keep a local copy)."""
        return self.driver.get_attr("filter-keep-local-copy", "checked") == "true"

    def set_filter_keep_local_copy(self, keep: bool) -> None:
        """Set the open filter form's `filter-keep-local-copy` checkbox,
        clicking only when it differs, then confirm the flip landed."""
        from helpers.waiting import wait_until

        if self.filter_keep_local_copy() != keep:
            self.driver.click("filter-keep-local-copy")
        wait_until(
            lambda: self.filter_keep_local_copy() == keep,
            5,
            diagnose=lambda: f"filter-keep-local-copy never read {keep}",
        )

    def filter_forward_address(self) -> str:
        """The open filter form's `filter-forward-address` buffer."""
        return self.driver.get_text("filter-forward-address")

    def open_filter_edit(self, index: int) -> None:
        """Open the nth filter's edit form (pre-populated via filters_get),
        returning once the form is up in edit mode."""
        self._navigate_subpage("privacy")
        self.driver.click("filter-edit", index=index)
        self.driver.wait_for("save-filter")

    def save_open_filter(self, timeout: float = 15) -> None:
        """Click `save-filter` on the open edit form and return once the save
        landed — the form closes (leaves edit mode) only on a successful
        `fauna.email.filters.update`; a refused save keeps it open and puts
        the reason on `error-message`."""
        from helpers.waiting import wait_until

        self.driver.click("save-filter")
        wait_until(
            lambda: not self.driver.is_visible("save-filter"),
            timeout,
            diagnose=lambda: "save-filter stayed open; error-message="
            + (self.driver.get_text("error-message")
               if self.driver.is_visible("error-message") else "<none>"),
        )

    def edit_filter(self, index: int, name: str, rule_type: str, rule_value: str,
                     action: str) -> None:
        """Edit the nth filter: open its edit form (pre-populated via
        filters_get) and overwrite every field, then save."""
        self._navigate_subpage("privacy")
        self.driver.click("filter-edit", index=index)
        self.driver.wait_for("save-filter")
        self.driver.clear_and_type("filter-name-input", name)
        self.driver.select("filter-rule-type", rule_type)
        self.driver.clear_and_type("filter-rule-value", rule_value)
        self.driver.select("filter-action-select", action)
        self.driver.click("save-filter")
        time.sleep(1)

    # --- Account info ---

    def actor_id(self) -> str:
        """Get the displayed actor ID."""
        if not self.driver.is_visible("account-actor-id"):
            return ""
        return self.driver.get_text("account-actor-id")

    def copy_actor_id(self) -> None:
        """Click the copy actor ID button."""
        self.driver.click("account-actor-id-copy-btn")

    def copy_status_actor_id(self) -> None:
        """Click the copy actor ID button on the status view."""
        self.driver.click("status-actor-id-copy-btn")

    def copy_status_node_url(self) -> None:
        """Click the copy node URL button on the status view."""
        self.driver.click("status-node-url-copy-btn")

    def navigate_to_account(self) -> None:
        """Click the account settings link."""
        self.driver.click("account-settings-link")

    def leave_account(self) -> None:
        """Leave the Account sub-page through the page's own way out — the
        acknowledgment gesture of `settings.md` § Recovery kit → *The
        persist-failure message survives the page*.

        The desktop shells exit through `settings-nav-back`. Mobile keeps the
        platform's idiomatic back (iOS: a NavigationStack pop — `settings-nav-back`
        is a declared optional absence in `ui-actual-ios.yaml`), which a plain
        Settings navigate performs: it lands on the root page list, off Account.
        """
        if self.driver.is_mobile():
            self.driver.navigate_to("settings")
        else:
            self.driver.click("settings-nav-back")

    # --- Recovery kit (settings.md § Recovery kit) ---

    def open_recovery_kit(self) -> None:
        """Navigate to the Account sub-page and wait for the section.

        Waits on the create button rather than the status line: the section
        renders its actions immediately and the line only once the read of the
        registration chain resolves, so the button is the element whose
        presence means "the section is up".
        """
        self._navigate_subpage("account")
        self.driver.wait_for("recovery-kit-create-button", timeout=10.0)

    def open_recovery_kit_or_skip(self) -> None:
        """Open Recovery Kit, or skip declaring the class if the section never lands.

        Does the navigation itself, on purpose — a caller that navigated first
        and gate-checked second would hard-fail with a ``TimeoutError`` on an
        app lacking the section instead of skipping, reporting a broken
        succession/kit journey where the truth is an unbuilt surface. Lifted
        from ``test_identity_succession_aftermath.py``'s local
        ``_require_recovery_section`` (2026-08-22) so every recovery-kit
        journey test shares one gate instead of five separate copies.
        """
        try:
            self.open_recovery_kit()
            return
        except Exception:
            pass
        from helpers.app_surface import skip_unbuilt

        skip_unbuilt(
            self.driver,
            surface="recovery-kit-section",
            detail=(
                "settings.md § Recovery kit; tui leads and the other six "
                "follow in batched trickle-down"
            ),
            tracked=(
                "docs/goal/behavior/identity-succession.md "
                "§ Implementation status today"
            ),
        )

    def recovery_kit_status(self) -> str:
        """The one status line, or "" before the read resolves.

        An un-hydrated section must not *claim* a state, but it still owes the
        user a reason for the four dead ceremony buttons (``ui/README.md`` rule
        5), so it paints ``status_loading`` in that window rather than nothing.
        That line is not an answer, so it reads as "" here — which keeps this
        method usable as the causal barrier for "the chain read completed"
        (convention 14; ``test_recovery_kit_create_journey`` polls exactly this).
        """
        # Scrolled: the line sits above the ceremony buttons and the minted panel, so
        # on a long Settings page it can be one scroll away — where windows'
        # `IsOffscreen` reads it as not there and this returns "" for a line that
        # is painted (a positive read; the section is built whenever this runs).
        if not self.driver.is_visible_scrolled("recovery-kit-status"):
            return ""
        text = self.driver.get_text("recovery-kit-status")
        return "" if text == S.settings.recovery_kit.status_loading else text

    def sweep_status(self) -> str:
        """The succession ceremony's sweep line — what the sweep did — or "".

        Renders only after a ceremony ran on this app run, at the top of the
        Recovery kit section, and stays silent on an account with no groups
        (``settings.md`` § Recovery kit → *The sweep's own lines*: the arm is
        ``SweepView::copy``'s, never the app's) — so "" is a legitimate answer.
        """
        if not self.driver.is_visible_scrolled("recovery-kit-sweep-status"):
            return ""
        return self.driver.get_text("recovery-kit-sweep-status")

    def sweep_unvouched_status(self) -> str:
        """The sweep's OTHER line — the roster it cannot vouch for — or "".

        Its own element, never a qualifier on :meth:`sweep_status`: the two
        facts have no combined verdict. "" whenever the roster is empty.
        """
        if not self.driver.is_visible_scrolled("recovery-kit-sweep-unvouched-status"):
            return ""
        return self.driver.get_text("recovery-kit-sweep-unvouched-status")

    def aftermath_mls_reseal_status(self) -> str:
        """Aftermath leg 3's progress line (`__mls` state-replica re-seal), or "".

        Renders only for an identity that succeeded from another AND had a
        replica at rest, and stays silent on the two nothing-was-owed arms — so
        "" is a legitimate answer, not only a not-yet-rendered one.

        ⚠ Unlike leg 2 this line has FIVE states: the extra one is
        *partly-owed*, a pass that unlocked some conversations and left others
        sealed to an identity this device cannot open. A test asserting "done"
        must compare against the done string exactly, never merely check the
        line is non-empty — partly-owed is progress, not completion.
        """
        if not self.driver.is_visible("recovery-kit-mls-reseal-status"):
            return ""
        return self.driver.get_text("recovery-kit-mls-reseal-status")

    def aftermath_corpus_reseal_status(self) -> str:
        """The owner-root corpus re-seal's progress line, or "".

        Folded app-side from the sync agent's own records
        (``succession-aftermath.md`` § Implementation status today), so it
        moves on the agent's status poll rather than on a sign-in. "" for an
        identity that never succeeded AND while no record has arrived yet —
        so "" is a legitimate answer, not proof the pass ran. Its one arm a
        journey can lean on is *owed elsewhere*: rendered exactly when this
        device is a successor holding no predecessor material at all.
        """
        if not self.driver.is_visible_scrolled("recovery-kit-corpus-reseal-status"):
            return ""
        return self.driver.get_text("recovery-kit-corpus-reseal-status")

    def aftermath_backup_regrant_status(self) -> str:
        """Aftermath leg 2's progress line (`NestBackupKey` re-grant), or "".

        ⚠ This is the **only** witness that the re-grant ran at the successor's
        post-auth hook. Asserting it via the Backups page instead would be
        vacuous: `read_backup_status` heals a missing enrollment on mount
        (`backup_enroll.rs`), so merely *opening* Backups makes the projection
        correct whether or not the aftermath leg ever ran.
        """
        if not self.driver.is_visible("recovery-kit-backup-regrant-status"):
            return ""
        return self.driver.get_text("recovery-kit-backup-regrant-status")

    def aftermath_grant_remint_status(self) -> str:
        """Aftermath leg 4's progress line (capability-grant re-mint), or "".

        Renders only for an identity that succeeded from another, and stays
        silent on the nothing-was-owed arms — so "" is a legitimate answer, not
        only a not-yet-rendered one. Its four states are running / done / owed
        elsewhere / failed.
        """
        if not self.driver.is_visible("recovery-kit-grant-remint-status"):
            return ""
        return self.driver.get_text("recovery-kit-grant-remint-status")

    def aftermath_mail_burn_status(self) -> str:
        """Aftermath leg 6's progress line (the MSEK burn), or "".

        Renders only for an identity that succeeded from another AND owed a
        burn, and stays silent on BOTH nothing-was-owed arms — including the
        one that still *writes* (a successor with no mail plane records the burn
        so a later "Enable mail" is not mistaken for predecessor-era material).
        So "" is a legitimate answer, not only a not-yet-rendered one.

        ⚠ This is the only aftermath line whose DONE state is not good news: it
        reports that every mail app password has been revoked on purpose. A test
        asserting the leg ran must read what the line SAYS — that mail apps need
        setting up again — because a line that merely exists could be the
        owed-elsewhere arm, which means the exposure is still open.
        """
        if not self.driver.is_visible("recovery-kit-mail-burn-status"):
            return ""
        return self.driver.get_text("recovery-kit-mail-burn-status")

    # --- The PERMANENT member-review page (ui.yaml page `member_review`) ---
    #
    # The other half of the two-surface review split
    # (`behavior/succession-aftermath.md` § Propagation, ruling 1). The
    # ephemeral kit-side pass renders inside the Recovery Kit section above and
    # only during the ceremony's own app run; everything it left unanswered
    # lives here afterwards. Both surfaces paint the SAME ids, so these helpers
    # deliberately do not name a page — they read whatever surface is on screen,
    # which is what lets one assertion be reused on either.

    def open_member_review(self) -> None:
        """Navigate to the permanent Members To Review sub-page.

        No `wait_for` on a row: the page's ordinary state is EMPTY (it holds
        only a backlog somebody explicitly postponed), so the honest barrier is
        the page heading, and a caller that expects rows polls for them itself.
        """
        self._navigate_subpage("member-review")

    def member_review_row_count(self) -> int:
        """How many people are still awaiting a verdict on the surface shown."""
        return self.driver.count("member-review-row")

    def member_review_row_text(self, index: int = 0) -> str:
        return self.driver.get_text("member-review-row", index=index)

    def member_review_row_has_both_verdicts(self, index: int = 0) -> bool:
        """Whether row `index` offers Keep AND Remove, both scoped inside it.

        Scoped rather than counted globally: two rows and two buttons somewhere
        on the page is exactly the state an unscoped pair produces, and it acts
        on whichever person happens to be first.
        """
        row = f"member-review-row[{index}]"
        return self.driver.is_visible(
            "member-review-keep-button", scope=row
        ) and self.driver.is_visible("member-review-remove-button", scope=row)

    def member_review_is_empty(self) -> bool:
        """Whether the permanent page is showing its empty state.

        ⚠ Not a safety verdict, and a test must not read it as one: an empty
        review list says nothing about whether the account is safe
        (`succession-aftermath.md` § Implementation status today leaves no
        combined "is the user safe" boolean for a surface to round up to).
        """
        return not self.driver.is_absent("member-review-empty")

    def member_review_keep(self, index: int = 0) -> None:
        """Press *Keep* on row `index` — scoped INSIDE that row, so the press
        always lands on the person the row names."""
        self.driver.click(
            "member-review-keep-button", scope=f"member-review-row[{index}]"
        )

    def member_review_remove(self, index: int = 0) -> None:
        """Press *Remove* on row `index`.

        ⚠ The verdict this earns is DERIVED, never chosen: a partial eviction
        earns none, the item stays open and **the row stays**, with the reason
        on the page's `error-message`. A caller must therefore never assert that
        the row disappears — assert what the surface says.
        """
        self.driver.click(
            "member-review-remove-button", scope=f"member-review-row[{index}]"
        )

    def member_review_defer_visible(self) -> bool:
        """Whether *Review The Rest Later* is on screen — the EPHEMERAL pass's
        own affordance; the permanent page never renders it (deferring is what
        sent the backlog there)."""
        return self.driver.is_visible("member-review-defer-button")

    def member_review_defer(self) -> None:
        """Press *Review The Rest Later* — hides the ephemeral pass and decides
        nothing: the open items stay exactly where they are, inherited by the
        permanent page."""
        self.driver.click("member-review-defer-button")

    def create_recovery_kit(self) -> None:
        """Run the create ceremony — registers a kit AND escrows the seed.

        ``wait_until_enabled`` first, because this control carries an
        ``isEnabled:`` predicate (``status.allowsCreate && !vm.busy``) whose
        inputs arrive from an **async chain read**: the section paints as soon as
        it mounts, and its status — for a caller who just switched identities,
        the *previous* account's status — is only replaced when that read lands.
        So the bare ``click`` drove a control no user could have clicked yet, and
        did it in the one window where the answer is not merely early but wrong.

        Latency-independent, never a settle-sleep: a generous named ceiling paid
        only on red (convention 14), and a control still disabled at the deadline
        is a real finding the raised ``TimeoutError`` names, with the section's
        own status line beside it. Measured on
        `test_identity_succession_ceremony.py --app macos`: every test in the file passed alone and two failed in one
        invocation, purely because a warm app reached this line sooner.
        """
        try:
            self.driver.wait_until_enabled(
                "recovery-kit-create-button", timeout=RECOVERY_KIT_READY_WAIT_S)
        except TimeoutError as e:
            # Convention 6: the failure diagnoses itself. `create` is enabled in
            # exactly ONE state (`allows_create` is `matches!(self,
            # NeverCreated)`), so a control still disabled at the deadline is the
            # section reporting a *registered* chain — and WHICH registered state
            # it reports is the whole diagnosis. Read the status line the section
            # paints, so the failure says "it thinks this account already has a
            # kit" rather than only "disabled".
            raise TimeoutError(
                f"{e}; recovery-kit-status reads "
                f"{self.recovery_kit_status()!r} — `create` is enabled ONLY "
                f"in the never-created state, so anything else here means the "
                f"section is reporting a chain that is not this account's fresh "
                f"one"
            ) from e
        self.driver.click("recovery-kit-create-button")

    def replace_lost_kit(self) -> None:
        """Run the seed-alone replacement — opens the 30-day window rather than
        taking effect now (`identity-succession.md` § The RecoveryKey →
        *Replacement*).

        Enabled in the registered states only, so a caller runs
        :meth:`create_recovery_kit` first. Like create, the ceremony mints a kit
        and shows its secret once — the window governs when it *lands*, not
        when it is generated.
        """
        self.driver.click("recovery-kit-lost-button")

    def enter_recovery_phrase(self, phrase: str) -> None:
        """Type a held kit into `recovery-entry-phrase-field`.

        The ONBOARDING `recovery_entry` screen's own id, reused inline in the
        Settings section (`settings.md` § Recovery kit → *Kit-in-hand entry*,
        user-approved 2026-08-01) — a pasted phrase is the same artifact as a
        displayed one, so no settings-scoped twin exists. Renders only while a
        kit-in-hand ceremony can consume it, which is why callers run it after
        a kit is registered.
        """
        self.driver.fill("recovery-entry-phrase-field", phrase)

    def replace_kit_with_held(self, phrase: str) -> None:
        """Replace the registered kit using one the user holds.

        Drives `create_kit`'s `prior` arm, which re-puts the escrow blob in the
        SAME ceremony (`identity-succession.md` § Seed escrow makes that a
        requirement: the nest deletes the escrow row the moment a registration
        changes the pubkey). Like create, it mints and shows a secret once.
        """
        self.enter_recovery_phrase(phrase)
        self.driver.click("recovery-kit-replace-button")

    def reseal_escrow_with_held(self, phrase: str) -> None:
        """Restore phrase recovery from the kit in hand — the no-escrow repair.

        Renders only in the *registered, no escrow* state (`settings.md`
        § Recovery kit). It re-puts the sealed copy under the kit the user
        holds and mints nothing, so — unlike every other ceremony here — no
        secret comes back to read.
        """
        self.enter_recovery_phrase(phrase)
        self.driver.click("recovery-kit-escrow-reseal-button")

    def veto_pending_replacement(self, phrase: str) -> None:
        """Cancel a pending seed-alone replacement, proving the kit you hold.

        Renders only while a replacement window is open (`settings.md`
        § User actions).
        """
        self.enter_recovery_phrase(phrase)
        self.driver.click("recovery-pending-veto-button")

    def copy_recovery_kit(self) -> None:
        """Press copy on the kit a ceremony just showed.

        What lands on the clipboard is the `fauna://recovery` URI, not the bare
        hex on screen — it names the account, which is what lets a restore
        find it with nothing typed (`identity-succession.md` § The RecoveryKey
        → *Kit payload*). Read it back with ``driver.get_clipboard_text()``.
        """
        self.driver.click("recovery-kit-secret-copy-btn")

    def succeed_identity_with_held_kit(self, phrase: str) -> None:
        """Take the account back from a stolen secret, using the kit in hand.

        The irreversible ceremony (`settings.md` § Recovery kit): it re-points
        the whole account to a freshly minted successor, so it is gated on
        `identity-stolen-confirm-field` reading the literal `"SUCCEED"` as well
        as on the phrase — the same type-to-confirm idiom account deletion uses.
        Both are filled here because a caller that filled only one would be
        testing the gate, not the ceremony.

        The token is NOT localized (only its prompt is), so this literal is the
        same on every app and in every language.

        On success this ends the session it was driven from: the nest revokes
        the old identity's bearers inside the succession transaction and the app
        re-launches as the successor, so a caller waits on the *new* signed-in
        surface rather than on anything this section renders.

        That revocation is why this action refuses to run against an identity
        the whole run shares (`helpers/shared_identity.py`): aimed at the
        session `test_user`, it signs out every later test in the run and none
        of their failures says so. The refusal is a backstop — the authoring
        shape is already refused at collection by
        `tests/test_no_succession_on_the_shared_identity.py`.
        """
        from helpers.shared_identity import refuse_ceremony_on_shared_identity

        refuse_ceremony_on_shared_identity(
            self.driver, ceremony="succeed_identity_with_held_kit"
        )
        self.enter_recovery_phrase(phrase)
        self.driver.fill("identity-stolen-confirm-field", "SUCCEED")
        self.driver.click("identity-stolen-button")

    def recovery_kit_secret(self) -> str:
        """The 64-hex secret a ceremony just minted, shown once, or "".

        There is no path that shows it again and there can never be one
        (`identity-succession.md` § The RecoveryKey — *Custody*), so a test
        that needs it must read it while it is on screen.
        """
        if not self.driver.is_visible_scrolled("recovery-kit-secret-display"):
            return ""
        return self.driver.get_text("recovery-kit-secret-display")

    # --- Quotas ---

    def is_quota_section_visible(self) -> bool:
        """Check if the quota section is visible."""
        return self.driver.is_visible("quota-section")

    def quota_inbox(self) -> str:
        """Get the inbox quota text."""
        if not self.driver.is_visible("quota-inbox"):
            return ""
        return self.driver.get_text("quota-inbox")

    def quota_storage(self) -> str:
        """Get the storage quota text."""
        if not self.driver.is_visible("quota-storage"):
            return ""
        return self.driver.get_text("quota-storage")

    def quota_devices(self) -> str:
        """Get the device count quota text."""
        if not self.driver.is_visible("quota-devices"):
            return ""
        return self.driver.get_text("quota-devices")

    # --- Feature limits (dynamic-features.md § Transparency & auditability) ---

    def wait_for_feature_limits(self, timeout: float = 15.0) -> None:
        """Wait for `feature-limits-section` — i.e. for the transparency read.

        The section registers only once `fauna.features.status` +
        `fauna.nest.info` resolve, so this waits on real data rather than on a
        paint (the `quota-section` contract). Generous budget, deadline poll —
        never a settle-sleep (`e2e-conventions.md` point 14)."""
        self.driver.wait_for("feature-limits-section", timeout=timeout)

    def feature_limit_row(self, index: int) -> dict[str, str]:
        """One registry member's row, read scoped to that row.

        Scoped, never a flat indexed read: several members are rendered at
        once, and a global query would silently answer with another feature's
        cells (`testing.md` point 1)."""
        scope = f"feature-limits-row[{index}]"
        row = {
            "name": self.driver.get_text("feature-limits-name", scope=scope),
            "status": self.driver.get_text("feature-limits-status", scope=scope),
        }
        # Scrolled, not a bare `is_visible`: the why-line sits below the row's name
        # and status, and on a long Status page a rendered line one scroll away
        # reads as absent to windows' `IsOffscreen` — which graded a correct build
        # red (`test_a_feature_the_admin_turned_off_shows_restricted_with_its_reason`).
        if self.driver.is_visible_scrolled("feature-limits-restriction", scope=scope):
            row["restriction"] = self.driver.get_text(
                "feature-limits-restriction", scope=scope
            )
        # The self-limits control's summary (dynamic-features.md § Authoring
        # surfaces) — read only where an app paints it, so this reader stays
        # usable on the apps still lifting the authoring half.
        if self.driver.count("feature-limits-own-summary", scope=scope) > 0:
            row["own_summary"] = self.driver.get_text(
                "feature-limits-own-summary", scope=scope
            )
        return row

    def open_own_feature_limit_editor(self, index: int) -> None:
        """Open the shared feature-policy editor in place for row `index` at the
        SELF tier (`feature-limits-own-edit-button`)."""
        self.driver.click(
            "feature-limits-own-edit-button", scope=f"feature-limits-row[{index}]"
        )

    def wait_for_feature_limit_row(
        self, index: int, predicate, timeout: float = 20.0
    ) -> dict[str, str]:
        """Poll row `index` until `predicate(row)` holds; return the last-seen row
        either way so the caller's assert carries it. A save re-reads the section
        without a re-navigation, so the row's DATA changes under a section that
        was already visible — poll the value, never the section."""
        deadline = time.monotonic() + timeout
        row = self.feature_limit_row(index)
        while time.monotonic() < deadline:
            if predicate(row):
                return row
            time.sleep(0.2)
            row = self.feature_limit_row(index)
        return row

    def feature_limit_quota(self, row: int, cell: int) -> dict[str, str]:
        """One (dimension, window) cell inside one member's row.

        Nested scope — the row, then the cell — which is what the indexed
        `feature-limits-quota` view exists for."""
        scope = f"feature-limits-row[{row}]/feature-limits-quota[{cell}]"
        return {
            "label": self.driver.get_text("feature-limits-quota-label", scope=scope),
            "value": self.driver.get_text("feature-limits-quota-value", scope=scope),
            "tier": self.driver.get_text("feature-limits-quota-tier", scope=scope),
        }

    def wait_for_feature_limit_quota_value(
        self, row: int, cell: int, expected_value: str, timeout: float = 10.0
    ) -> dict[str, str]:
        """Poll `feature_limit_quota` until its `value` field reads
        `expected_value`, or return the last-seen cell at the deadline.

        `feature-limits-section` is a one-time-per-session marker — it stays
        visible across a re-nav that only refreshes its DATA (`wait_for_feature_limits`
        would return instantly against the section's prior, now-stale
        content). Needed after any nav edge that re-fetches
        `fauna.features.status` — the round trip is a real
        network read, not a paint, so the value can genuinely still be in
        flight the instant the section itself is already visible from an
        earlier fetch. Deadline poll, never a settle-sleep
        (`e2e-conventions.md` point 14)."""
        deadline = time.monotonic() + timeout
        cell_data = self.feature_limit_quota(row, cell)
        while time.monotonic() < deadline:
            if cell_data["value"] == expected_value:
                return cell_data
            time.sleep(0.2)
            cell_data = self.feature_limit_quota(row, cell)
        return cell_data
