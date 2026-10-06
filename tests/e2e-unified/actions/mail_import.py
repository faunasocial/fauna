from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class MailImportActions:
    """Drive the user-facing mail-import wizard (docs/goal/behavior/
    mailbox-migration.md § UX shape; tests/e2e-unified/ui.yaml `mail-import` page).

    A person pulls mail from a foreign IMAP server (Gmail / Outlook / iCloud /
    generic) into **their own** Fauna mailbox here — a five-step wizard
    (Source → Scope → Confirm → Progress → Done) backed by the shared
    `fauna_client_mail_settings::MailImportMachine`, client-driven, credentials
    never touching nest. Unlike `MailExportActions`'s twin, the backend is REAL
    end-to-end (nest RPC surface + the foreign-IMAP-source client both ship).

    tui is the lead app for this page (`mailbox-migration.md` § Implementation
    status today — "tui first, per the lead-app ordering"); the other six lift
    this shape once they render it.

    Step 1's Source fields are conditional by provider kind — see
    `apps/fauna-tui/src/settings/mail_import.rs`'s module docs for the exact
    table (Gmail/iCloud show app-password; Outlook shows the OAuth button AND
    the IMAP-fallback fields; Generic shows the fallback fields alone).

    Scope↔Confirm navigation uses the shared `wizard-next-button` /
    `wizard-back-button` ids, and each mailbox row is an indexed
    `mail-import-scope-mailbox-item` whose text is the mailbox name and whose
    ``state`` attribute is ``on``/``off`` (all user-approved 2026-08-28).
    `MailExportActions` mirrors this identical shape (user-approved 2026-08-29).
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        """Navigate to the mail-import Settings sub-page (sidebar-swap shell on
        linux; the sub-id is ignored on single-scroll clients)."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "mail-import"}]},
        })

    def is_page_visible(self, timeout: float = 10.0) -> bool:
        """True when the mail-import wizard is reachable (source picker present)."""
        try:
            self.driver.wait_for("mail-import-source-picker", timeout=timeout)
            return True
        except TimeoutError:
            return False

    def error_text(self) -> str:
        """The page-level error-message text — a real Connect/Start/Pause/
        Resume/Cancel rejection, since the backend is real (unlike Export's
        permanent unbuilt-backend explanation).

        ``error-message`` is built hidden — absent from the registry, not
        merely empty — when there's nothing to show, so an unguarded
        `get_text` 404s on a clean page. Check visibility first.
        """
        if not self.driver.is_visible("error-message"):
            return ""
        try:
            return self.driver.get_text("error-message")
        except Exception:
            return ""

    # ── step 1 — source ──────────────────────────────────────────────────

    def select_source(self, label: str) -> None:
        """Pick the source provider (Gmail / Outlook / iCloud / Generic IMAP)."""
        self.driver.select("mail-import-source-picker", label)

    def set_username(self, value: str) -> None:
        self.driver.clear_and_type("mail-import-source-username", value)

    def set_app_password(self, value: str) -> None:
        """Gmail/iCloud only — see the module docs' per-provider table."""
        self.driver.clear_and_type("mail-import-source-app-password", value)

    def set_host(self, value: str) -> None:
        """Generic/Outlook-fallback only."""
        self.driver.clear_and_type("mail-import-source-host", value)

    def host_value(self) -> str:
        """The Source step host field's current draft text (Generic/Outlook-
        fallback only) — what a kind change seeds it to, or what the user typed."""
        return self.driver.get_text("mail-import-source-host")

    def set_port(self, value: str) -> None:
        """Generic/Outlook-fallback only."""
        self.driver.clear_and_type("mail-import-source-port", value)

    def port_value(self) -> str:
        """The Source step port field's current draft text (Generic/Outlook-
        fallback only) — what a kind change seeds it to, or what the user typed."""
        return self.driver.get_text("mail-import-source-port")

    def select_tls_mode(self, label: str) -> None:
        """Generic/Outlook-fallback only."""
        self.driver.select("mail-import-source-tls-mode", label)

    def set_password(self, value: str) -> None:
        """Generic/Outlook-fallback only."""
        self.driver.clear_and_type("mail-import-source-password", value)

    def connect(self) -> None:
        """Step 1→2: LOGIN + LIST against the source. On success advances to
        Scope; on failure stays on Source with `error_text()` set."""
        self.driver.wait_for("mail-import-connect-button", timeout=10.0)
        self.driver.click("mail-import-connect-button")

    # ── step 2 — scope ───────────────────────────────────────────────────

    _MAILBOX_ITEM = "mail-import-scope-mailbox-item"

    def mailbox_names(self) -> list[str]:
        """The source mailboxes offered on the Scope step, in painted order.

        The rows are an *indexed* element (convention 1), so the name is the
        row's text and position is the only handle a driver has on it.
        """
        return self.driver.get_texts(self._MAILBOX_ITEM)

    def _mailbox_index(self, name: str) -> int:
        names = self.mailbox_names()
        if name not in names:
            raise AssertionError(
                f"no source mailbox row named {name!r} on the Scope step; "
                f"offered: {names}"
            )
        return names.index(name)

    def mailbox_selected(self, name: str) -> bool:
        """Whether `name` is checked for import.

        Read from the row's ``state`` attribute rather than its glyph: the
        checkbox mark is a per-app rendering detail, the attribute is the
        contract every app serves.
        """
        return (
            self.driver.get_attr(
                self._MAILBOX_ITEM, "state", index=self._mailbox_index(name)
            )
            == "on"
        )

    def toggle_mailbox(self, name: str) -> None:
        self.driver.click(self._MAILBOX_ITEM, index=self._mailbox_index(name))

    def set_date_from(self, value: str) -> None:
        self.driver.clear_and_type("mail-import-scope-date-from", value)

    def set_max_size_mb(self, value: str) -> None:
        self.driver.clear_and_type("mail-import-scope-max-size", value)

    def scope_next(self) -> None:
        """Step 2→3: Scope→Confirm."""
        self.driver.wait_for("wizard-next-button", timeout=10.0)
        self.driver.click("wizard-next-button")

    def back(self) -> None:
        """Step 2→1 or 3→2 — the shared wizard Back."""
        self.driver.wait_for("wizard-back-button", timeout=10.0)
        self.driver.click("wizard-back-button")

    # ── step 3 — confirm / commit ────────────────────────────────────────

    def confirm_summary(self) -> str:
        return self.driver.get_text("mail-import-confirm-summary")

    def start(self) -> None:
        """Durable commit → MailImportAction::Start (opens the
        import_sessions row; the app glue spawns `run_import`)."""
        self.driver.wait_for("mail-import-start-button", timeout=10.0)
        self.driver.click("mail-import-start-button")

    # ── step 4 — progress ────────────────────────────────────────────────

    def _text_or_empty(self, element_id: str) -> str:
        """The element's text, or ``""`` when the wizard has not painted it yet.

        Every reader on this class is polled ACROSS a step transition — the
        caller is asking "has the import reached Done?" — so "not painted yet"
        is an ordinary state, not an error. But `/element/text` 404s on a widget
        it cannot find (deliberately: 200-ing empty made a
        missing element indistinguishable from an empty one), and the driver
        raises `LookupError` on that 404. A bare `get_text` in a `wait_until`
        predicate therefore dies on the FIRST poll instead of polling — and so
        does the `diagnose=` lambda that was supposed to explain the timeout,
        which is how the walk's first Done wait reported `LookupError: not
        found` with no diagnosis at all.
        """
        try:
            return self.driver.get_text(element_id)
        except LookupError:
            return ""

    def progress_summary(self) -> str:
        return self._text_or_empty("mail-import-progress-summary")

    def error_log(self) -> str:
        return self._text_or_empty("mail-import-error-log")

    def pause(self) -> None:
        self.driver.click("mail-import-pause-button")

    def resume(self) -> None:
        self.driver.click("mail-import-resume-button")

    def cancel(self) -> None:
        """Two-click inline confirm → MailImportAction::Cancel (already-
        imported messages are kept)."""
        self.driver.click("mail-import-cancel-button")  # arm
        self.driver.click("mail-import-cancel-button")  # confirm

    def mailbox_progress_count(self) -> int:
        return self.driver.count("mail-import-mailbox-progress-list-item-name")

    # ── step 5 — done ────────────────────────────────────────────────────

    def done_summary(self) -> str:
        return self._text_or_empty("mail-import-done-summary")

    def view_imported(self) -> None:
        self.driver.click("mail-import-view-imported-button")

    def review_skipped(self) -> None:
        self.driver.click("mail-import-review-skipped-button")
