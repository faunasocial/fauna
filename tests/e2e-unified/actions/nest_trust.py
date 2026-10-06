from __future__ import annotations

import time
from typing import TYPE_CHECKING

from i18n.strings import S

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class NestTrustActions:
    """Drive the nest-trust facet on the Nests page (docs/goal/ui/nests.md § Trust facet).

    The net-new v1 facet on each nest row: a per-row Now/History lens over the
    client-authoritative grant-event log, the current content-processing grants a
    nest is *trusted to read* (Now), that nest's grant-event timeline (History),
    per-grant renew/revoke controls, and — when a nest holds no grants — the
    `nest-trust-empty` "not trusted to read anything" state. Renders the shared
    trust-enabled `LinkedNestsMachine`; the shell holds no trust logic
    (priority #1/#2). Navigate to the surface first via
    `LinkedNestsActions.navigate()` (same page).

    The `nest-trust-*` ids are net-new — no `linked-nests-*`→`nests-*` prefix
    transition applies. The facet exists only on clients that have adopted the
    trust machine (web leads; the others as they migrate).

    v1 holder discovery is admin-scoped (nests.md § Implementation status); a nest
    with no enumerable content-processor holders (a fresh nest, no bridge) shows
    the empty state — which the slice-6 rendering test asserts. The
    mint → fetch → revoke functional flow is slice 7 (tier_3, needs a holder).
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def require_mint_test_setup_supported(self) -> None:
        """Skip unless this app builds the SELF Tiers-tab author sections
        (`subscription-tiers-section` & co.) this test's setup uses to seed a
        tier (e2e convention 7 — the platform check lives in the action
        layer, not the test body).

        trust-mint UI itself landed on linux (lead) + web + macos + ios +
        windows (nests.md § Mint, ratified 2026-07-13); tui built the mint
        flow too (2026-07-24, unit-proven in settings/nests.rs) and **landed
        the Tiers author page 2026-08-01** (the tui Tiers author track), so it seeds
        this test's tier through the same UI every other app does — widened
        that day, with no other change to the test, exactly as this docstring
        anticipated. android remains out (its e2e is host-emulator-gated
        fleet-wide)."""
        if not (
            self.driver.is_linux()
            or self.driver.is_web()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_windows()
            or self.driver.is_tui()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the SELF Tiers-tab author sections "
                        "(subscription-tiers-section & co.)",
                detail="needed to seed this test's tier; tui's mint flow "
                       "itself is built and unit-proven, but tui has no "
                       "Tiers author page yet",
                tracked="ui-actual-tui.yaml (Tiers author page, future slice)",
            )

    def require_backup_trust_rows_supported(self) -> None:
        """Skip unless this app renders backup-destination trust rows (e2e
        convention 7 — the platform check lives in the action layer, not the
        test body).

        Built on linux (lead) + web + tui (2026-07-24) + macos + ios
        (2026-07-29, shared FaunaKit `LinkedNestsView`) + windows (2026-08-24,
        NestsPanel.xaml `nest-trust-backup-item`) + android (its
        `LinkedNestsScreen.kt` already implements `BackupItem` with all
        `nest-trust-backup-*` testTags). android is excluded below anyway —
        its e2e is host-emulator-gated fleet-wide, not a missing feature."""
        if not (
            self.driver.is_linux()
            or self.driver.is_web()
            or self.driver.is_tui()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_windows()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="backup-destination trust rows",
                detail="android has it but is excluded here for the same "
                       "fleet-wide host-emulator e2e gate as every other "
                       "android test",
                tracked="nests.md § Trust facet",
            )

    def require_retained_generations_supported(self) -> None:
        """Skip unless this app renders retained-generation rows (e2e
        convention 7 — the platform check lives in the action layer, not the
        test body).

        tui is the lead app for this facet (testing.md § Default app and
        nest mode) and renders it in direct Rust with no FFI hop; linux
        carries the same shared projection. macos/ios joined 2026-08-02 (one
        shared FaunaKit view, `LinkedNestsView.swift`'s `GenerationItemView`),
        web landed 2026-08-02 too (`NestsSection.svelte` — no shared-Rust/wasm
        work owed, the projection already crossed the wasm boundary on tui's
        landing), and windows landed 2026-08-24 (`Controls/NestsPanel.xaml`'s
        `nest-trust-generation-item`, the last of 7 shells). android also
        renders it (`LinkedNestsScreen.kt`) but is excluded below anyway —
        its e2e is host-emulator-gated fleet-wide, same as every other
        android test."""
        if not (
            self.driver.is_linux()
            or self.driver.is_tui()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_web()
            or self.driver.is_windows()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="retained-generation rows",
                detail="android has it but is excluded here for the same "
                       "fleet-wide host-emulator e2e gate as every other "
                       "android test",
                tracked="nests.md § Trust facet — generation recovery",
            )

    def lens_toggle_present(self, timeout: float = 8.0) -> bool:
        """True once a nest row's Now/History lens toggle is on screen."""
        try:
            self.driver.wait_for("nest-trust-view-now", timeout=timeout)
            return self.driver.is_visible("nest-trust-view-history")
        except TimeoutError:
            return False

    def is_empty_state_visible(self, timeout: float = 8.0) -> bool:
        """True once a nest shows the `nest-trust-empty` "not trusted" state.

        The POSITIVE poll only — the negative direction is `empty_state_absent`."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.is_visible("nest-trust-empty"):
                return True
            time.sleep(0.3)
        return False

    def empty_state_absent(self, window: float = 1.0) -> bool:
        """True iff `nest-trust-empty` reads ABSENT for the whole `window` (the
        negative twin of `is_empty_state_visible`)."""
        return self._absent_throughout("nest-trust-empty", window)

    def _absent_throughout(self, element_id: str, window: float) -> bool:
        """The negative twin of a "wait until visible" poll: False the moment
        `element_id` reads PRESENT, True once it has read absent for `window`.

        Reads through `driver.is_absent` — the one primitive a negative read
        uses (convention 6's rider) — because a poll on `is_visible` returns
        "not there" for an element that IS in the tree but off the viewport, so
        a `not poll(...)` assertion passes over a control that is on screen
        (`e2e-conventions.md` § The conventions, convention 6). The window is a
        named ceiling on how long a late-rendering element is given to show up,
        never a subject."""
        deadline = time.monotonic() + window
        while True:
            if not self.driver.is_absent(element_id):
                return False
            if time.monotonic() >= deadline:
                return True
            time.sleep(0.3)

    def empty_state_text(self) -> str:
        """The rendered empty-state copy (`nest-trust-empty`)."""
        return self.driver.get_text("nest-trust-empty") or ""

    def grant_count(self) -> int:
        """How many current grants render in the Now lens (`nest-trust-grant-item`)."""
        return self.driver.count("nest-trust-grant-item")

    def history_count(self) -> int:
        """How many grant-event rows render in the History lens
        (`nest-trust-history-item`)."""
        return self.driver.count("nest-trust-history-item")

    def history_texts(self) -> list[str]:
        """Every History-lens row's self-describing line, most recent first
        (`nests.md` § Trust facet — History lens) — "Trusted to read …",
        "Trust renewed: …", "Trust revoked: …"."""
        return [
            self.driver.get_text("nest-trust-history-item", index=j) or ""
            for j in range(self.history_count())
        ]

    def show_history(self, index: int = 0, timeout: float = 10.0) -> None:
        """Flip a nest row's lens to History (`nest-trust-view-history`) →
        dispatches SetLens (local UI state, no nest round-trip), and return once
        the row paints the History lens: its `nest-trust-history-list`
        container renders in that lens only, on every app, even for an empty
        timeline. Waiting on that state instead of a fixed pause is what keeps
        the Now-only assertions after it honest on a loaded box (convention
        14)."""
        self.driver.click("nest-trust-view-history", index=index)
        self._wait_for_lens(index, history=True, timeout=timeout)

    def show_now(self, index: int = 0, timeout: float = 10.0) -> None:
        """Flip a nest row's lens back to Now (`nest-trust-view-now`), and
        return once the History container is gone from that row."""
        self.driver.click("nest-trust-view-now", index=index)
        self._wait_for_lens(index, history=False, timeout=timeout)

    def _wait_for_lens(self, index: int, *, history: bool, timeout: float) -> None:
        scope = f"nests-item[{index}]"
        deadline = time.monotonic() + timeout
        while True:
            present = self.driver.count("nest-trust-history-list", scope=scope) > 0
            if present == history:
                return
            if time.monotonic() >= deadline:
                raise TimeoutError(
                    f"nest row {index} never painted the "
                    f"{'History' if history else 'Now'} lens within {timeout}s "
                    f"(nest-trust-history-list present={present}); "
                    f"{self.driver.diagnose('nest-trust-history-list', scope=scope)}"
                )
            time.sleep(0.2)

    # ── mint flow (scope-first picker, design ratified 2026-07-13) ──────────

    @staticmethod
    def paywalled_label(tier: str) -> str:
        """The rendered scope-select label for a per-tier paywalled-posts option
        (the generated i18n string — the same constant the apps render, so
        the select-by-label contract can't drift from copy changes)."""
        return S.nests.mint_option_paywalled(tier=tier)

    MAIL_LABEL = S.nests.mint_option_mail
    CALENDAR_LABEL = S.nests.mint_option_calendar

    def mint_button_present(self, timeout: float = 8.0) -> bool:
        """True once the mint affordance (`nest-trust-grant-mint-button`) is on
        screen. Hidden when the row's mint_options catalog is empty (nothing
        derivable / no content-processor holder / non-admin discovery).

        The POSITIVE poll only: an `is_visible` read is the wrong tool for the
        negative direction (see `mint_button_absent`)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.is_visible("nest-trust-grant-mint-button"):
                return True
            time.sleep(0.3)
        return False

    def mint_button_absent(self, window: float = 1.0) -> bool:
        """True iff the mint affordance reads ABSENT for the whole `window` —
        the negative twin of `mint_button_present`, and the read a "nothing
        derivable, so nothing to mint" assertion must use: it gates offering a
        capability grant at all, so a read a viewport can fool (windows'
        `is_visible` is `!IsOffscreen`) would pass while the button was shown."""
        return self._absent_throughout("nest-trust-grant-mint-button", window)

    def open_mint(self, index: int = 0) -> None:
        """Open the mint flow (`nest-trust-grant-mint-button`) — reveals the
        scope select + confirm."""
        self.driver.click("nest-trust-grant-mint-button", index=index)
        time.sleep(0.5)

    def select_scope(self, label: str, index: int = 0) -> None:
        """Pick a use-case option in `nest-trust-mint-scope-select` by its
        rendered label (`MAIL_LABEL` / `CALENDAR_LABEL` / `paywalled_label(t)`)."""
        self.driver.select("nest-trust-mint-scope-select", label, index=index)
        time.sleep(0.3)

    def holder_select_visible(self) -> bool:
        """True when the conditional `nest-trust-mint-holder-select` renders —
        only for a scope option with >1 holder candidate (none today)."""
        return not self.driver.is_absent("nest-trust-mint-holder-select")

    def confirm_mint(self, index: int = 0) -> None:
        """Confirm the mint (`nest-trust-mint-confirm-button`) → dispatches
        Mint{nest_id, holder_bridge_id, scope} and refreshes the facet."""
        self.driver.click("nest-trust-mint-confirm-button", index=index)
        time.sleep(1)

    # ── duration + blessing (nests.md § Expiry / renewal → Duration and
    # blessing). Built on tui, linux, web, android, macOS and iOS; windows mints
    # the standard window with no picker and no toggle until its lift.

    ONE_OFF_LABEL = S.nests.mint_duration_one_off
    STANDARD_LABEL = S.nests.mint_duration_standard

    def _has_duration_and_blessing(self) -> bool:
        """tui led; linux, web and android followed (2026-09-28), then macOS
        and iOS. windows still mints the standard window with neither control."""
        d = self.driver
        return (d.is_tui() or d.is_linux() or d.is_web() or d.is_android()
                or d.is_macos() or d.is_ios())

    def require_duration_and_blessing_supported(self) -> None:
        """Skip unless this app renders `nest-trust-mint-duration-select` and
        `nest-trust-blessed-toggle` and runs the auto-renew tick (convention 7 —
        the platform check lives here, not in the test body)."""
        if not self._has_duration_and_blessing():
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the mint duration picker and the nest blessing toggle",
                detail="the shared machine is in place; this app's shell "
                       "does not render the two controls yet",
                tracked="nests.md § Implementation status today",
            )

    def select_duration(self, label: str, index: int = 0) -> None:
        """Pick how long the new trust lasts in the open mint form
        (`nest-trust-mint-duration-select`) by its rendered label
        (`ONE_OFF_LABEL` / `STANDARD_LABEL`)."""
        self.driver.select("nest-trust-mint-duration-select", label, index=index)

    def pick_standard_duration_where_offered(self, index: int = 0) -> None:
        """Choose the standard ~90-day window in the open mint form — a real
        pick where the app renders the duration picker, nothing elsewhere:
        the apps without it always mint the standard window (their shells name
        it on `Mint`), so the resulting grant is the same. Setup for journeys
        that need a standing grant, not a skip."""
        if self._has_duration_and_blessing():
            self.select_duration(self.STANDARD_LABEL, index=index)

    def duration_selected(self, index: int = 0) -> str:
        """The duration the open mint form shows — the row's default until
        the user picks."""
        return self.driver.get_text(
            "nest-trust-mint-duration-select", index=index
        ) or ""

    def blessed_state(self, nest_index: int = 0) -> str:
        """`"on"` / `"off"` — the blessing toggle's state on nest row
        `nest_index` (`nest-trust-blessed-toggle`, the toggle convention's
        `state` attribute)."""
        return self.driver.get_attr(
            "nest-trust-blessed-toggle", "state", scope=f"nests-item[{nest_index}]"
        ) or ""

    def set_blessed(self, blessed: bool, nest_index: int = 0, timeout: float = 20.0) -> None:
        """Bless (or un-bless) nest row `nest_index` from its toggle and wait
        for the row to say so — a deadline poll on the rendered state, never a
        settle-sleep (convention 14)."""
        want = "on" if blessed else "off"
        if self.blessed_state(nest_index) != want:
            self.driver.click(
                "nest-trust-blessed-toggle", scope=f"nests-item[{nest_index}]"
            )
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.blessed_state(nest_index) == want:
                return
            time.sleep(0.3)
        raise AssertionError(
            f"the blessing toggle never read {want!r}; "
            f"now {self.blessed_state(nest_index)!r}"
        )

    def wait_for_grant_count(self, expected: int, timeout: float = 12.0) -> bool:
        """Poll the Now lens until `expected` grant rows render (the mint's
        deposit + log append + re-list round-trip is async)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.grant_count() >= expected:
                return True
            time.sleep(0.5)
        return self.grant_count() >= expected

    def grant_scope_text(self, index: int = 0) -> str:
        """The rendered scope line of the grant row at `index`
        (`nest-trust-grant-scope`, "Trusted to read: …")."""
        return self.driver.get_text("nest-trust-grant-scope", index=index) or ""

    def grant_status_text(self, index: int = 0) -> str:
        """The grant row's liveness state (`nest-trust-grant-status`) — active /
        expiring soon / expired ("paused — renew to resume") / auto-renewing
        (`nests.md` § Expiry / renewal — first-class states)."""
        return self.driver.get_text(
            "nest-trust-grant-status", scope=self._grant_scope(index)
        ) or ""

    def grant_bound_note_text(self, index: int = 0) -> str:
        """The REQUIRED honest-bound copy on a CONTENT grant row
        (`nest-trust-grant-bound-note`, `nests.md` § Honest bound) — the grant
        twin of `backup_bound_note_text`."""
        return self.driver.get_text(
            "nest-trust-grant-bound-note", scope=self._grant_scope(index)
        ) or ""

    def has_grant_renew(self, index: int = 0) -> bool:
        """Whether the grant row at `index` offers `nest-trust-grant-renew` —
        the recovery an expired grant must keep offering."""
        return not self.driver.is_absent(
            "nest-trust-grant-renew", scope=self._grant_scope(index)
        )

    def wait_for_grant_status(
        self, expected: str, index: int = 0, timeout: float = 20.0
    ) -> str:
        """Re-hydrate the Nests page until the grant row at `index` reads
        `expected`, returning the last status seen.

        The render clock is read on the page's nav-edge hydrate, so each poll
        re-navigates; the budget is a named ceiling on a state wait, never a
        settle-sleep (convention 14)."""
        from actions.linked_nests import LinkedNestsActions

        nav = LinkedNestsActions(self.driver)
        deadline = time.monotonic() + timeout
        last = ""
        while time.monotonic() < deadline:
            nav.navigate()
            if self.grant_count() > index:
                last = self.grant_status_text(index)
                if last == expected:
                    return last
            time.sleep(0.5)
        return last

    def require_trust_clock_supported(self) -> None:
        """Skip unless this app carries the trust facet's render-clock seam —
        the `trust_facet_advance_clock` agent command over
        `fauna_client_capabilities::trust_clock` (e2e convention 7 — the
        platform check lives in the action layer, not the test body).

        The clock itself is shared Rust and already feeds every native leg's
        fold (and web's grant fold); tui, linux, web, android, macOS and iOS
        expose the command (the apple pair through the shared FaunaKit
        `TrustFacetClockTestCommand`). windows still owes its agent arm over
        the UniFFI `set_trust_clock_offset_secs` door (testing.md § Default
        app and nest mode)."""
        d = self.driver
        if not (d.is_tui() or d.is_linux() or d.is_web() or d.is_android()
                or d.is_macos() or d.is_ios()):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the trust facet's render-clock agent command "
                        "(trust_facet_advance_clock)",
                detail="the shared clock and its UniFFI door are in place; "
                       "this app's automation agent does not route the "
                       "command yet",
                tracked="nests.md § Implementation status today",
            )

    def advance_trust_clock(self, offset_secs: int) -> None:
        """Move the trust facet's RENDER clock (grant liveness + custody receipt
        freshness; never the mint/renew clock) by `offset_secs`. The next Nests
        navigate paints the moved state. Pass `0` to reset — the offset is
        process-wide and nothing auto-resets it."""
        self.driver.call_command(
            "trust_facet_advance_clock",
            {"now_offset_secs": offset_secs},
            timeout=30,
        )

    # ── custody rows + the escrow-holder badge (nests.md § Trust facet —
    # custody rows; participants.md § The participant model). Rendered on tui,
    # linux, web and android (nests.md § Implementation status today); the one
    # journey that reads them mints its custody through the Devices-page
    # ceremony, so it gates through `CustodyActions.require_supported()` AND
    # `require_custody_rows_supported()` below — an app can drive the ceremony
    # before it renders the custodian-nest row it produces.

    def require_custody_rows_supported(self) -> None:
        """Skip unless this app renders the custodian-NEST rows
        (`nest-trust-custody-*`) and the escrow-holder badge on its Nests page
        (e2e convention 7 — the platform check lives in the action layer).

        tui, linux, web, android, macOS and iOS render them (apple via the
        shared FaunaKit Nests page, 2026-09-29). windows drives the custody
        ceremony (pieces 1 + 3 and the mint, 2026-09-28) but its Nests page does
        not render the custodian-nest rows yet."""
        d = self.driver
        if not (
            d.is_tui() or d.is_linux() or d.is_web() or d.is_android()
            or d.is_macos() or d.is_ios()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the Nests page's custodian-nest rows "
                        "(nest-trust-custody-*) and the escrow-holder badge",
                detail="the shared fold carries the rows; this app's Nests "
                       "page does not paint them yet",
                tracked="nests.md § Implementation status today",
            )

    def custody_count(self) -> int:
        """How many custodian-NEST rows render (`nest-trust-custody-item`)."""
        return self.driver.count("nest-trust-custody-item")

    def custody_receipt_status_text(self, index: int = 0) -> str:
        """The custodian row's receipt line (`nest-trust-custody-receipt-status`)
        — fresh ("Last confirmed …"), stale ("Stale — last confirmed …") or
        "No confirmation yet": three states, three different words."""
        return self.driver.get_text(
            "nest-trust-custody-receipt-status", index=index
        ) or ""

    def escrow_badge_text(self, nest_index: int = 0) -> str:
        """The escrow-holder role badge on nest row `nest_index`
        (`participant-escrow-holder-badge`), or `""` when that row does not
        hold the account's recovery escrow — ABSENT, not empty, is the
        not-a-holder state."""
        scope = f"nests-item[{nest_index}]"
        if self.driver.is_absent("participant-escrow-holder-badge", scope=scope):
            return ""
        return self.driver.get_text(
            "participant-escrow-holder-badge", scope=scope
        ) or ""

    def revoke(self, index: int = 0) -> None:
        """Revoke the grant at `index` (`nest-trust-grant-revoke`) →
        `fauna.capabilities.revoke` + a signed Revoke event; the holder goes
        dark. Used by the slice-7 functional flow."""
        self.driver.click("nest-trust-grant-revoke", index=index)
        time.sleep(1)

    def renew(self, index: int = 0) -> None:
        """Renew the grant at `index` (`nest-trust-grant-renew`) →
        `fauna.capabilities.renew` + a signed Renew event (window bump)."""
        self.driver.click("nest-trust-grant-renew", index=index)
        time.sleep(1)

    # ── post-succession review: the per-row Keep/Revoke pair
    # (succession-aftermath.md § Re-key scope → *Adjudicating what the aftermath
    # carries across*). The STRICTER of the two adjudication planes — a
    # thief-added backup destination still only receives segments sealed under a
    # key it lacks, while a thief-added grantee is handed live read capability by
    # the successor's own client.

    # ⚠ **The scope is the FULL two-step chain, and a partial one silently reads
    # absent.** A grant leaf registers under BOTH its containers — `nests-item[i]`
    # then `nest-trust-grant-item[j]` (`settings/nests.rs`'s `nests_elements` doc,
    # the shape `nests.md:53` documents) — and tui's registry matches an entry
    # only when the query scope is a **prefix of the entry's path from the root**
    # (`automation.rs::Registry::matches`). So `nest-trust-grant-item[j]` alone
    # matches nothing: the entry's first step is `nests-item[i]`. It fails the
    # worst possible way — `is_visible` answers False for a mark that is painted
    # on screen, which reads exactly like the product never rendered it. That
    # cost row 56 two full journey runs and a RED pin (2026-08-11); the home nest
    # is row 0 (`nests.md` § Layout, "home first"), hence the default.
    def _grant_scope(self, index: int, nest_index: int = 0) -> str:
        """The full ancestor chain of one grant row's leaves — see the ⚠ above."""
        return f"nests-item[{nest_index}]/nest-trust-grant-item[{index}]"

    def grant_unattested_mark_visible(self, index: int = 0) -> bool:
        """Whether the grant row at `index` carries
        `nest-trust-grant-unattested-mark`.

        ⚠ **Absence, not emptiness, is the un-raised state** — the same rule the
        backups plane's twin follows, and for the same reason (`ui.yaml`'s
        registry entry: *"Absent — not empty — on an ordinary row"*). Assert
        ``False``, never an empty string.
        """
        return not self.driver.is_absent(
            "nest-trust-grant-unattested-mark",
            scope=self._grant_scope(index),
        )

    def grant_unattested_mark_text(self, index: int = 0) -> str:
        """The raised grant row's review copy, `""` when the row is genuinely
        not raised, or `<unreadable: ...>` when the mark is present but its
        text could not be read.

        ⚠ **Diagnostic-only — no assertion reads this** (assert
        `grant_unattested_mark_visible` instead; see its own docstring for
        why). But a diagnostic that answers `""` for a failed read is exactly
        the misleading kind convention 6 forbids: it reads identically to
        "the mark was never raised", masking a real read failure behind the
        genuinely-absent case (`e2e-self-diagnosing-failures.md` § The
        convention). `is_absent` shares this method's own scope, so it asks
        the SAME question the failed read couldn't answer — never a second
        guess at absence.
        """
        scope = self._grant_scope(index)
        try:
            return self.driver.get_text(
                "nest-trust-grant-unattested-mark",
                scope=scope,
            )
        except Exception as exc:
            if self.driver.is_absent("nest-trust-grant-unattested-mark", scope=scope):
                return ""
            return f"<unreadable: {type(exc).__name__}>"

    def grant_keep_visible(self, index: int = 0) -> bool:
        """Whether the grant row at `index` offers `nest-trust-grant-keep-button`."""
        return not self.driver.is_absent(
            "nest-trust-grant-keep-button",
            scope=self._grant_scope(index),
        )

    def keep_grant(self, index: int = 0) -> None:
        """Press **Keep** on the raised grant row at `index`.

        Records a `Kept` verdict against the mark in the `fauna.state.succession-ledger` plane
        (`keep_grant_mark` — the verdict is *recorded*, never deleted, so an
        answered mark stays distinguishable from one never raised) and refreshes
        the facet. The Revoke half has no twin: `nest-trust-grant-revoke` above
        already is it.
        """
        self.driver.click(
            "nest-trust-grant-keep-button",
            scope=self._grant_scope(index),
        )

    # ── backup trust rows (ratified 2026-07-24; nests.md § Trust facet — backup
    # rows). Two row kinds share `nest-trust-backup-item`: the NestBackupKey
    # seal grant and one row per configured backup destination. Rendered in the
    # Now lens after the content-grant rows, on the HOME nest's row only — both
    # grants empower the source nest.

    def backup_row_count(self) -> int:
        """How many backup trust rows render (`nest-trust-backup-item`)."""
        return self.driver.count("nest-trust-backup-item")

    def wait_for_backup_row_count(self, expected: int, timeout: float = 20.0) -> bool:
        """Poll until at least `expected` backup rows render.

        Generous budget, deadline-polled: a green run pays only the real
        latency, and the read behind these rows makes a live round trip per
        configured destination (testing.md convention 14 — assert
        latency-independent state, never wall-clock timing).
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.backup_row_count() >= expected:
                return True
            time.sleep(0.5)
        return self.backup_row_count() >= expected

    def backup_scope_text(self, index: int = 0) -> str:
        """The rendered scope line of the backup row at `index`
        (`nest-trust-backup-scope`)."""
        return self.driver.get_text("nest-trust-backup-scope", index=index) or ""

    def backup_status_text(self, index: int = 0) -> str:
        """The backup row's state (`nest-trust-backup-status`) — active /
        unreachable / missing."""
        return self.driver.get_text("nest-trust-backup-status", index=index) or ""

    def backup_since_text(self, index: int = 0) -> str:
        """The backup row's "trusted since" line (`nest-trust-backup-since`).
        EMPTY on the seal row — that grant carries no timestamp on the wire
        (nests.md:67) — so an empty string here is a real expected value, not a
        lookup failure."""
        return self.driver.get_text("nest-trust-backup-since", index=index) or ""

    def backup_bound_note_text(self, index: int = 0) -> str:
        """The REQUIRED honest-bound copy (`nest-trust-backup-bound-note`)."""
        return self.driver.get_text("nest-trust-backup-bound-note", index=index) or ""

    def revoke_backup(self, index: int = 0) -> None:
        """Press the freeze-the-backup affordance on the backup row at `index`
        (`nest-trust-backup-revoke`). The shared machine routes it: the seal row
        to the source nest (`fauna.backup.nest_key.revoke`), a writer row to
        **the destination** over its own connection
        (`fauna.backup.writer_grant.revoke`) — the shell only names the row."""
        self.driver.click("nest-trust-backup-revoke", index=index)

    # ── retained generations (ratified 2026-07-29; nests.md § Trust facet —
    # generation recovery). Rendered on the HOME nest's row AFTER the backup
    # rows: what the owner can roll back to inside the custody grace window T.
    # The list is FLATTENED across destinations — one row per retained
    # generation, plus exactly one row per destination that could not be asked.

    def generation_count(self) -> int:
        """How many retained-generation rows render
        (`nest-trust-generation-item`)."""
        return self.driver.count("nest-trust-generation-item")

    def wait_for_generation_count(self, expected: int, timeout: float = 20.0) -> bool:
        """Poll until at least `expected` generation rows render.

        Same generous-budget deadline poll as `wait_for_backup_row_count`: the
        read behind these rows makes a live round trip per configured
        destination, so a green run pays only the real latency and no assertion
        here depends on wall-clock timing (testing.md convention 14).
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.generation_count() >= expected:
                return True
            time.sleep(0.5)
        return self.generation_count() >= expected

    def generation_path_text(self, index: int = 0) -> str:
        """The generation row's identity line (`nest-trust-generation-path`) —
        the plaintext path when the custody row carried one, else the
        `path_hash`. A path-less row is NEVER hidden (nests.md:123), so a hash
        here is a real expected value."""
        return self.driver.get_text("nest-trust-generation-path", index=index) or ""

    def generation_status_text(self, index: int = 0) -> str:
        """The generation row's state (`nest-trust-generation-status`) — listed
        / unreachable. "Could not reach" is deliberately NOT the same as
        "nothing to restore" (nests.md:122)."""
        return self.driver.get_text("nest-trust-generation-status", index=index) or ""

    def generation_expires_text(self, index: int = 0) -> str:
        """The generation row's deadline line (`nest-trust-generation-expires`),
        which also carries the REQUIRED quota-bound copy."""
        return self.driver.get_text("nest-trust-generation-expires", index=index) or ""

    def generation_size_text(self, index: int = 0) -> str:
        """The generation's size (`nest-trust-generation-size`)."""
        return self.driver.get_text("nest-trust-generation-size", index=index) or ""

    def has_generation_restore(self, index: int = 0) -> bool:
        """Whether the row at `index` offers a restore affordance. An
        `unreachable` row must NOT (nests.md:122) — there is no address to
        restore, and offering it would imply knowledge we do not have."""
        return self.driver.count("nest-trust-generation-restore") > index

    def restore_generation(self, index: int = 0) -> None:
        """Press `nest-trust-generation-restore` on the row at `index`. The
        shared machine speaks it to **the destination** over that destination's
        own connection, carrying the row's own address triple — never through
        the source nest, which is the writer being recovered from."""
        self.driver.click("nest-trust-generation-restore", index=index)

    def generation_notice_text(self) -> str:
        """The restore-outcome notice (`nest-trust-generation-notice`,
        ratified 2026-07-29) — the HOME row's own element, NOT indexed (a
        restore's outcome is the page's last action, not any one generation
        row). Empty until a restore resolves. Distinct from `error-message`:
        `PastRecoveryWindow` is a product state, never an error, so it never
        rides that channel."""
        return self.driver.get_text("nest-trust-generation-notice") or ""
