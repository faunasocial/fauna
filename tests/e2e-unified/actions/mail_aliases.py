from __future__ import annotations

import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class MailAliasesActions:
    """Drive the user-facing mail-aliases page (docs/goal/behavior/mail-aliases.md
    § Aliases UX; tests/e2e-unified/ui.yaml `mail-aliases` page).

    A person manages **their own** per-account mail addresses here — the canonical
    `<handle>@<domain>` exact alias, extra exact aliases, wildcard prefixes, and
    one-click disposable mints — backed by the shared
    `fauna_client_mail_settings::MailAliasesMachine` over the User-class
    `fauna.bridges.{list,create,update,revoke,delete}_account_alias` +
    `generate_disposable_alias` surface.

    The page is reached via the settings/status view on every app (the
    adw::PreferencesWindow modal is not reachable through the state protocol), so
    navigating to "settings" surfaces the mail-aliases IDs. Action methods are
    kept uniform across all 7 apps — linux is the
    lead; the other five lift this shape.

    The add-sheet (and per-row controls) are inline reveals, not modals — the
    linux state protocol can't open a separate window (same idiom as the
    mail-settings add-credential form). The kind picker is a single value-via-
    state toggle (unchecked = Exact, checked = Wildcard); disposable aliases mint
    via the dedicated generate button, not the add-sheet.
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        """Navigate to the mail-aliases Settings sub-page (sidebar-swap shell on
        linux; the sub-id is ignored on single-scroll clients)."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "mail-aliases"}]},
        })

    def is_page_visible(self, timeout: float = 10.0) -> bool:
        """True when the mail-aliases page is reachable (add-button present).

        Uses wait_for so the driver scrolls the button into the viewport — the
        aliases page sits below the mail-settings section in the embedded
        settings view, so GTK may not have rendered it into the AT-SPI tree
        until it's scrolled on-screen.
        """
        try:
            self.driver.wait_for("mail-aliases-add-button", timeout=timeout)
            return True
        except TimeoutError:
            return False

    # ── list reads ────────────────────────────────────────────────────────

    def row_count(self) -> int:
        """How many alias rows the list currently renders."""
        return self.driver.count("mail-aliases-list-item-pattern")

    def patterns(self) -> list[str]:
        """The rendered display address of every alias row, in order."""
        return [
            self.driver.get_text("mail-aliases-list-item-pattern", i)
            for i in range(self.row_count())
        ]

    def index_of_pattern(self, address: str) -> int:
        """Row index whose pattern element renders `address`, or -1 if absent."""
        for i, p in enumerate(self.patterns()):
            if p == address:
                return i
        return -1

    def has_pattern(self, address: str) -> bool:
        return self.index_of_pattern(address) >= 0

    def wait_for_row_count(self, expected: int, timeout: float = 12.0) -> bool:
        """Poll until the list holds exactly `expected` rows (the dispatch →
        snapshot → re-render round-trip is async)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.row_count() == expected:
                return True
            time.sleep(0.3)
        return self.row_count() == expected

    def wait_for_pattern(self, address: str, timeout: float = 12.0) -> bool:
        """Poll until a row rendering `address` appears."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.has_pattern(address):
                return True
            time.sleep(0.3)
        return self.has_pattern(address)

    def wait_for_pattern_gone(self, address: str, timeout: float = 12.0) -> bool:
        """Poll until no row renders `address`."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self.has_pattern(address):
                return True
            time.sleep(0.3)
        return not self.has_pattern(address)

    # ── mutations ─────────────────────────────────────────────────────────

    def refresh_via_generate(self) -> None:
        """Force a list refresh by minting a disposable — through the enabled
        control, the way a user would.

        `generate_disposable_alias` derives <handle>+<domain> server-side from
        the actor's canonical exact alias (no client-side precondition —
        `mail-aliases.md:82`), and its success path re-lists every alias, so one
        mint both proves the disposable flow and pulls a seeded alias into the
        rendered snapshot (setting `default_domain`).

        ⚠ This used to be a bare `wait_for` + `click`, and that is exactly how a
        real deadlock hid for months: the page hydrated once at login, so a
        client that enabled mail afterwards had `default_domain` unset — which
        on linux disabled add, generate AND import together, leaving the page
        with no way forward from the UI while these tests kept driving the
        disabled button directly (convention 8; found by the permissive
        actuation sweep, fixed 2026-08-28). The
        `wait_until_enabled` is not politeness: it is the assertion that a user
        could have performed this action at all.
        """
        self.driver.wait_until_enabled(
            "mail-aliases-generate-disposable-button", timeout=15.0
        )
        self.driver.click("mail-aliases-generate-disposable-button")

    def disposable_count(self) -> int:
        """How many rows render a disposable address (the `-temp-` tag)."""
        return sum(1 for p in self.patterns() if "-temp-" in p)

    def wait_for_disposable_count(self, minimum: int, timeout: float = 12.0) -> bool:
        """Poll until at least `minimum` rows render a disposable address.

        `wait_for_pattern` is exact-match, and a disposable's 6-character token
        is minted server-side, so the address is not known to the caller — this
        is the `-temp-` counterpart of that wait.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.disposable_count() >= minimum:
                return True
            time.sleep(0.3)
        return self.disposable_count() >= minimum

    def add_exact(
        self,
        pattern: str,
        label: str = "",
        *,
        spam_threshold: int | None = None,
        rate_per_hour: int | None = None,
    ) -> None:
        """Open the add-sheet, fill an exact-alias pattern (+ optional label,
        spam-threshold override and hourly limit), and submit — dispatching
        MailAliasesAction::Create{kind:Exact}.

        The kind toggle stays unchecked (Exact is the default); only wildcard
        create flips it.
        """
        self.driver.wait_for("mail-aliases-add-button", timeout=10.0)
        self.driver.click("mail-aliases-add-button")
        self.driver.wait_for("mail-aliases-add-sheet-pattern-input", timeout=10.0)
        self.driver.clear_and_type("mail-aliases-add-sheet-pattern-input", pattern)
        if label:
            self.driver.clear_and_type("mail-aliases-add-sheet-label-input", label)
        self._fill_controls(spam_threshold, rate_per_hour)
        self.driver.click("mail-aliases-add-sheet-submit-button")

    def _fill_controls(self, spam_threshold: int | None, rate_per_hour: int | None) -> None:
        """Type the sheet's two optional per-alias controls (mail-aliases.md
        § Per-alias controls); `None` leaves a field as it is."""
        if spam_threshold is not None:
            self.driver.clear_and_type(
                "mail-aliases-add-sheet-spam-threshold-input", str(spam_threshold)
            )
        if rate_per_hour is not None:
            self.driver.clear_and_type(
                "mail-aliases-add-sheet-rate-per-hour-input", str(rate_per_hour)
            )

    def open_edit(self, index: int) -> None:
        """Open the edit sheet for the row at `index` (the add sheet's
        structure, pre-populated; mail-aliases.md § Layout)."""
        self.driver.click(
            "mail-aliases-list-item-edit-button", scope=self._row_scope(index)
        )
        self.driver.wait_for("mail-aliases-add-sheet-pattern-input", timeout=10.0)

    def sheet_value(self, element_id: str) -> str:
        """The current text of one of the open sheet's inputs."""
        return self.driver.get_text(element_id)

    def kind_picker_disabled(self) -> bool:
        """Whether the open sheet's kind picker is read-only (it is on edit:
        a wildcard cannot become a disposable mid-life)."""
        return self.driver.get_attr("mail-aliases-add-sheet-kind-picker", "disabled") == "true"

    def submit_edit(
        self,
        *,
        label: str | None = None,
        spam_threshold: int | None = None,
        rate_per_hour: int | None = None,
    ) -> None:
        """Change the open edit sheet's label / controls and submit —
        dispatching MailAliasesAction::Update."""
        if label is not None:
            self.driver.clear_and_type("mail-aliases-add-sheet-label-input", label)
        self._fill_controls(spam_threshold, rate_per_hour)
        self.driver.click("mail-aliases-add-sheet-submit-button")

    def row_text(self, element_id: str, index: int) -> str:
        """The text of one row leaf (`-label`, `-kind`, `-hits`) at row `index`."""
        return self.driver.get_text(element_id, scope=self._row_scope(index))

    def minted_confirmation(self) -> str | None:
        """The address the page confirms it copied after a disposable mint,
        read off the generate button's `copied` attribute (the copy-button
        contract `account-actor-id-copy-btn` set: OSC 52 and most clipboards
        are fire-and-forget, so the confirmation carries what was copied)."""
        return self.driver.get_attr("mail-aliases-generate-disposable-button", "copied")

    def add_wildcard(self, prefix: str, label: str = "") -> None:
        """Open the add-sheet, select Wildcard, fill the prefix, and submit —
        dispatching MailAliasesAction::Create{kind:Wildcard}."""
        self.driver.wait_for("mail-aliases-add-button", timeout=10.0)
        self.driver.click("mail-aliases-add-button")
        self.driver.wait_for("mail-aliases-add-sheet-kind-picker", timeout=10.0)
        self.driver.click("mail-aliases-add-sheet-kind-picker")  # Exact → Wildcard
        self.driver.clear_and_type("mail-aliases-add-sheet-pattern-input", prefix)
        if label:
            self.driver.clear_and_type("mail-aliases-add-sheet-label-input", label)
        self.driver.click("mail-aliases-add-sheet-submit-button")

    def add_disposable(self, *, ttl_days: int, uses: int, label: str = "") -> None:
        """Open the add-sheet, pick Disposable, choose how long it lasts and how
        many messages it takes, and submit — dispatching
        MailAliasesAction::GenerateDisposable with those values
        (mail-aliases.md § Layout). The picker steps Exact → Wildcard →
        Disposable; its `kind` attr is the selection read back."""
        import time

        self.driver.wait_for("mail-aliases-add-button", timeout=10.0)
        self.driver.click("mail-aliases-add-button")
        self.driver.wait_for("mail-aliases-add-sheet-kind-picker", timeout=10.0)
        deadline = time.monotonic() + 10.0
        while (
            self.driver.get_attr("mail-aliases-add-sheet-kind-picker", "kind") != "disposable"
            and time.monotonic() < deadline
        ):
            self.driver.click("mail-aliases-add-sheet-kind-picker")
        assert self.driver.get_attr("mail-aliases-add-sheet-kind-picker", "kind") == "disposable"
        self.driver.wait_for("mail-aliases-add-sheet-ttl-input", timeout=10.0)
        self.driver.clear_and_type("mail-aliases-add-sheet-ttl-input", str(ttl_days))
        self.driver.clear_and_type("mail-aliases-add-sheet-uses-input", str(uses))
        if label:
            self.driver.clear_and_type("mail-aliases-add-sheet-label-input", label)
        self.driver.click("mail-aliases-add-sheet-submit-button")

    def cancel_add(self) -> None:
        self.driver.click("mail-aliases-add-sheet-cancel-button")

    def generate_disposable(self) -> None:
        """One-click disposable mint via the dedicated button."""
        self.driver.wait_for("mail-aliases-generate-disposable-button", timeout=10.0)
        self.driver.click("mail-aliases-generate-disposable-button")

    def _row_scope(self, index: int) -> str:
        """Scope path for the row at `index` (the `patterns()`/
        `index_of_pattern()` index space).

        The canonical `<handle>@<domain>` row omits every mutating control
        (mail-aliases.md:249), so those controls' own occurrence-index
        compresses relative to the row index the moment a canonical row
        precedes the target — a raw `click(control, index)` would then hit
        the wrong row. Scoping to the row itself sidesteps the two index
        spaces entirely (testing.md point 1: scoped queries, never global
        counts).
        """
        return f"mail-aliases-list-item[{index}]"

    def revoke(self, index: int) -> None:
        """Soft-revoke the alias at `index` (flips disabled=true, preserves the
        row).

        Per-app the gesture behind this one id differs: linux/tui wire it as a
        genuine two-click inline arm-then-confirm (`wire_two_click`,
        `apps/fauna-linux/src/settings/mail_aliases.rs:944`); apple dispatches
        Revoke on EVERY click (`MailAliasesView.swift:222-229`, no arm state),
        so the second click here just re-revokes an already-revoked alias — an
        idempotent no-op, never a double-effect, because Revoke only ever sets
        disabled=true. The gap between the two clicks is not a correctness
        wait (the caller deadline-polls the actual `disabled=true` ground
        truth over WS-RPC after this returns) — it exists only so linux/tui's
        re-render lands before the second click re-targets the same id.
        """
        scope = self._row_scope(index)
        self.driver.click("mail-aliases-list-item-revoke-button", scope=scope)
        time.sleep(0.3)  # sleep-ok: inter-click pacing for an app-dependent
        # single/double-dispatch gesture; the caller polls real ground truth,
        # this never gates correctness (see docstring above).
        self.driver.click("mail-aliases-list-item-revoke-button", scope=scope)

    def delete(self, index: int) -> None:
        """Destructively delete the alias at `index` via the overflow-menu's
        Delete. Two-click inline confirm (no modal), the first tap relabelling
        to `common.confirm_q` ("Confirm?"), on every app but android, whose
        overflow menu is a real dropdown whose Delete item deletes on one tap
        (apps/common.md § Two-click confirm records the gap). The inter-click gap is pacing for that re-render, not a
        correctness wait — the caller deadline-polls `wait_for_pattern_gone`
        against ground truth after this returns."""
        self.arm_delete(index)
        time.sleep(0.3)  # sleep-ok: inter-click pacing for the arm→confirm
        # re-render; the caller polls real ground truth, this never gates
        # correctness (see docstring above).
        self.confirm_delete(index)

    def arm_delete(self, index: int) -> None:
        """First press of the row's two-click Delete: arms it, deletes nothing."""
        self.driver.click("mail-aliases-list-item-overflow-menu", scope=self._row_scope(index))

    def confirm_delete(self, index: int) -> None:
        """Second press, while armed: dispatches the Delete."""
        self.driver.click("mail-aliases-list-item-overflow-menu", scope=self._row_scope(index))

    def delete_button_text(self, index: int) -> str:
        """The row's Delete button label — it relabels once armed, which is the
        only visible affordance of the two-click confirm."""
        return self.row_text("mail-aliases-list-item-overflow-menu", index)

    def import_addresses(self, lines: list[str]) -> None:
        """Open the bulk paste-import sheet (`mail-aliases-import-sheet`,
        opened by `mail-aliases-import-button`), paste `lines` (one address per
        line) into the textarea, and submit — dispatching
        `MailAliasesAction::Import` -> `fauna.bridges.import_account_aliases`.
        Mirrors `add_exact` (open + wait + fill + submit) and
        `MailListMembersActions.batch_import`'s shape."""
        self.driver.click("mail-aliases-import-button")
        self.driver.wait_for("mail-aliases-import-textarea", timeout=10.0)
        self.driver.clear_and_type("mail-aliases-import-textarea", "\n".join(lines))
        self.driver.click("mail-aliases-import-submit-button")

    def cancel_import(self) -> None:
        self.driver.click("mail-aliases-import-cancel-button")

    def read_import_result(self, timeout: float = 10.0) -> str:
        """Return the rendered `mail-aliases-import-result` per-line outcome
        summary once it appears, else "" (poll — the import round-trip is
        async, same idiom as `page_error_text`)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.is_visible("mail-aliases-import-result"):
                return self.driver.get_text("mail-aliases-import-result")
            time.sleep(0.3)
        return ""

    def toggle_active(self, index: int) -> None:
        """Flip the per-row "Active" switch at `index`
        (`mail-aliases-list-item-disabled-toggle`). It is two-way: ON = enabled
        (receiving), OFF = disabled. Turning it off dispatches Revoke; turning it
        back on dispatches Enable (`fauna.bridges.enable_account_alias`). A
        freshly-created alias starts enabled (switch ON), so the first flip
        disables and the second re-enables."""
        self.driver.click(
            "mail-aliases-list-item-disabled-toggle", scope=self._row_scope(index)
        )

    # ── errors ────────────────────────────────────────────────────────────

    def page_error_text(self, timeout: float = 10.0) -> str:
        """Return the page's `error-message` text once it appears, else ""."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.is_visible("error-message"):
                return self.driver.get_text("error-message")
            time.sleep(0.3)
        return ""
