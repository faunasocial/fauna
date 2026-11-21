from __future__ import annotations

from typing import TYPE_CHECKING

from helpers.budgets import UI_SETTLE_S
from helpers.waiting import wait_until

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class MailExportActions:
    """Drive the user-facing mail-export wizard (docs/goal/behavior/mail-export.md
    § UX shape; tests/e2e-unified/ui.yaml `mail-export` page).

    A person exports **their own** mailbox here — a five-step wizard (Format →
    Scope → Confirm → Progress → Done) backed by the shared
    `fauna_client_mail_settings::MailExportMachine` over the (currently unbuilt)
    user-tier `fauna.bridges.{start,list,pause,resume,cancel,finalize}_export_session`
    + `discard_export_blob` surface + the `GET /api/v1/export/<id>` download path.

    The page is reached via the settings/status view on every app (the
    adw::PreferencesWindow modal is not reachable through the state protocol), so
    navigating to "settings" surfaces the mail-export IDs. Action methods are kept
    uniform across all 7 apps — linux is the lead; the
    other five lift this shape.

    Steps 1–3 (Format / Scope / Confirm) are client-side. Format↔Scope↔Confirm
    navigation uses the shared `wizard-next-button` / `wizard-back-button` ids,
    and each mailbox row is an indexed `mail-export-scope-mailbox-item` whose
    text is the mailbox name and whose ``state`` attribute is ``on``/``off``
    (all user-approved 2026-08-29, mirroring `MailImportActions`'s identical
    shape approved 2026-08-28). The durable commit is the tagged
    mail-export-start-button.
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def _press_live(self, element_id: str) -> None:
        """Wait for ``element_id`` to be live, then press it.

        A press the app refuses as disabled after the wait saw it live means
        the control went blocked in between; the refusal alone says only
        "disabled", so the failure names what the app painted at that moment —
        the control's enabled state and reason, the connection word and its
        report count (convention 6)."""
        self.driver.wait_until_enabled(element_id, timeout=UI_SETTLE_S)
        try:
            self.driver.click(element_id)
        except RuntimeError as e:
            if "element is disabled" not in str(e):
                raise
            raise AssertionError(
                f"{element_id} was live, then refused the press as disabled: "
                f"{self._why_blocked(element_id)}"
            ) from e

    def _why_blocked(self, element_id: str) -> str:
        parts = []
        try:
            parts.append(self.driver.diagnose(element_id, attrs=("enabled", "reason")))
        except Exception as e:  # noqa: BLE001 — diagnostic only
            parts.append(f"<diagnose: {type(e).__name__}: {e}>")
        for key in ("connection", "connection_reports"):
            try:
                parts.append(f"{key}={self.driver.get_state(key)!r}")
            except Exception as e:  # noqa: BLE001 — diagnostic only
                parts.append(f"{key}=<{type(e).__name__}: {e}>")
        try:
            parts.append(f"error-message={self.error_text()!r}")
        except Exception as e:  # noqa: BLE001 — diagnostic only
            parts.append(f"error-message=<{type(e).__name__}: {e}>")
        return " | ".join(parts)

    def navigate(self) -> None:
        """Navigate to the mail-export Settings sub-page (sidebar-swap shell on
        linux; the sub-id is ignored on single-scroll clients)."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "mail-export"}]},
        })

    def is_page_visible(self, timeout: float = 10.0) -> bool:
        """True when the mail-export wizard is reachable (format picker present)."""
        try:
            self.driver.wait_for("mail-export-format-picker", timeout=timeout)
            return True
        except TimeoutError:
            return False

    def error_text(self) -> str:
        """The page-level error-message text (the unbuilt-backend explanation
        until the export job model lands).

        ``error-message`` is built hidden — absent from the registry, not
        merely empty — when there's nothing to show (`ErrorBanner.swift`), so
        an unguarded `get_text` 404s on a clean page. Check presence first,
        with ``is_absent``, never ``is_visible`` — windows' ``is_visible`` is
        ``!IsOffscreen``, so an error bar scrolled above the viewport would read
        as ``""`` (`MailImportActions.error_text`, the same page shape).
        """
        if self.driver.is_absent("error-message"):
            return ""
        try:
            return self.driver.get_text("error-message")
        except Exception:
            return ""

    # ── step 1 — format ───────────────────────────────────────────────────

    def select_format(self, label: str) -> None:
        """Pick the export format from the picker (mbox / Maildir++ / EML zip).

        Returns once the picker shows ``label``: the apple picker dispatches the
        choice to the shared machine asynchronously, so its shown value trails
        the select by a beat. A picker that never shows it fails here."""
        self.driver.select("mail-export-format-picker", label)
        wait_until(
            lambda: self.selected_format() == label, 5.0, interval=0.1,
            diagnose=lambda: f"picker shows {self.selected_format()!r}, wanted {label!r}; "
            f"{self._why_blocked('mail-export-format-picker')}",
        )

    def selected_format(self) -> str:
        """The format the picker shows as chosen. Web's `get_text` answers a
        `<select>`'s `<option value>` (`Mbox`), so web reads the selected
        option's painted text — the label every other app's picker shows."""
        if self.driver.is_web():
            return (
                self.driver.get_attr("mail-export-format-picker", "selected-text") or ""
            ).strip()
        return self.driver.get_text("mail-export-format-picker")

    def format_options(self) -> list[str] | None:
        """Every format the picker paints, or ``None`` on a driver that does not
        serve the ``options`` attr (`drivers/base.py::option_texts`)."""
        return self.driver.option_texts("mail-export-format-picker")

    # (an id only one step paints, the id the step its Next opens paints) —
    # Format → Scope, Scope → Confirm.
    _NEXT_OPENS = (
        ("mail-export-format-picker", "mail-export-scope-strip-headers-toggle"),
        ("mail-export-scope-strip-headers-toggle", "mail-export-start-button"),
    )

    def next(self) -> None:
        """Step 1→2 or step 2→3 — the shared wizard Next (same id both steps).

        Waits for it to be live, as the person does: the Scope step's Next is
        painted from the machine's scope answer (`mail-export.md` § *Next and
        Start offer only an export that can start*) — blocked until the
        mailbox list has arrived — and every app paints a nest-bound control
        blocked while its connection is not up. A test that means to witness a
        blocked Next reads it with ``is_enabled`` instead.

        Returns once the next step has painted. The press reaches the machine
        as an asynchronous op (tui's ``Op::MailExportDispatch``), so the click
        answers while the old step is still on screen under the same Next id;
        a second ``next()`` straight after would read the Format step's Next as
        live, press it, and land the press on a Scope step whose Next is still
        blocked — refused, and the wizard stays on Scope. Where a step's id is
        not painted (or both are), the wait degrades to none."""
        self.driver.wait_until_enabled("wizard-next-button", timeout=UI_SETTLE_S)
        opens = next(
            (there for here, there in self._NEXT_OPENS if self.driver.is_visible(here)),
            None,
        )
        self._press_live("wizard-next-button")
        if opens is not None:
            self.driver.wait_for(opens, timeout=UI_SETTLE_S)

    def back(self) -> None:
        """Step 2→1 or step 3→2 — the shared wizard Back."""
        self.driver.wait_for("wizard-back-button", timeout=10.0)
        self.driver.click("wizard-back-button")

    # ── step 2 — scope ────────────────────────────────────────────────────

    _MAILBOX_ITEM = "mail-export-scope-mailbox-item"

    def mailbox_names(self) -> list[str]:
        """The source mailboxes offered on the Scope step, in painted order.

        The rows are an *indexed* element (convention 1), so the name is the
        row's text and position is the only handle a driver has on it.
        """
        # Web paints each row as a checkbox label, so its text carries the
        # label's leading space; a mailbox name is the trimmed text.
        return [t.strip() for t in self.driver.get_texts(self._MAILBOX_ITEM)]

    def _mailbox_index(self, name: str) -> int:
        names = self.mailbox_names()
        if name not in names:
            raise AssertionError(
                f"no mailbox row named {name!r} on the Scope step; offered: {names}"
            )
        return names.index(name)

    def mailbox_selected(self, name: str) -> bool:
        """Whether `name` is selected for export.

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
        self.driver.clear_and_type("mail-export-scope-date-from", value)

    def set_date_to(self, value: str) -> None:
        self.driver.clear_and_type("mail-export-scope-date-to", value)

    def toggle_strip_headers(self) -> None:
        """Flip the transit-header strip; returns once the toggle paints the
        flipped state. The press reaches the machine as an asynchronous op and
        the toggle reads back from its snapshot (never a click echo), so a read
        straight after the click can still show the old state."""
        # The Scope step paints a beat after the Next that opens it.
        self.driver.wait_for("mail-export-scope-strip-headers-toggle", timeout=10.0)
        was_on = self.strip_headers_on()
        self.driver.click("mail-export-scope-strip-headers-toggle")
        wait_until(
            lambda: self.strip_headers_on() != was_on, UI_SETTLE_S, interval=0.2,
            diagnose=lambda: (
                f"the strip toggle still reads {'on' if was_on else 'off'} after the "
                f"press; error-message: {self.error_text()!r}"
            ),
        )

    def strip_headers_on(self) -> bool:
        """Whether the transit-header strip is on — the toggle's ``state``
        attribute, the contract every app serves (as for the mailbox rows)."""
        self.driver.wait_for("mail-export-scope-strip-headers-toggle", timeout=10.0)
        return self.driver.get_attr("mail-export-scope-strip-headers-toggle", "state") == "on"

    # ── step 3 — confirm / commit ─────────────────────────────────────────

    def confirm_summary(self) -> str:
        return self.driver.get_text("mail-export-confirm-summary")

    def start(self) -> None:
        """Durable commit → MailExportAction::Start (opens the export_sessions row)."""
        self.driver.wait_for("mail-export-start-button", timeout=10.0)
        self.driver.click("mail-export-start-button")

    # ── step 4 — progress ─────────────────────────────────────────────────

    def progress_summary(self) -> str:
        return self.driver.get_text("mail-export-progress-summary")

    def pause(self) -> None:
        self.driver.click("mail-export-pause-button")

    def resume(self) -> None:
        self.driver.click("mail-export-resume-button")

    def cancel(self) -> None:
        """Two-click inline confirm → MailExportAction::Cancel."""
        self.arm_cancel()
        self.confirm_cancel()

    def arm_cancel(self) -> None:
        """The first Cancel click: asks for confirmation, cancels nothing."""
        self.driver.click("mail-export-cancel-button")

    def confirm_cancel(self) -> None:
        """The second Cancel click — the confirmation that cancels."""
        self.driver.click("mail-export-cancel-button")

    def cancel_button_text(self) -> str:
        return self.driver.get_text("mail-export-cancel-button")

    def mailbox_progress_count(self) -> int:
        return self.driver.count("mail-export-mailbox-progress-list-item-name")

    def mailbox_progress(self) -> list[tuple[str, str]]:
        """``[(mailbox, its progress text), …]`` in painted order — the
        per-mailbox rows of the Progress step."""
        names = self.driver.get_texts("mail-export-mailbox-progress-list-item-name")
        progress = self.driver.get_texts("mail-export-mailbox-progress-list-item-progress")
        return list(zip(names, progress))

    def error_log(self) -> str:
        """The Progress step's error log, as painted."""
        return self.driver.get_text("mail-export-error-log")

    # ── step 5 — done ─────────────────────────────────────────────────────

    def done_summary(self) -> str:
        return self.driver.get_text("mail-export-done-summary")

    def download(self) -> None:
        # Painted blocked while it cannot run — waited for, as the person does.
        self._press_live("mail-export-download-button")

    def discard(self) -> None:
        self.driver.click("mail-export-discard-button")
