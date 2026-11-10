from __future__ import annotations

import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class FamilyActions:
    """Drive the `family` page (family-safety.md § App surface) — the
    guardian section (wards + the ONE shared reach-policy editor + the
    approvals queue + contact-add + graduate) and the supervised section
    (guardian handle + a read-only policy summary). ui.yaml's `family:`
    block carries exactly one non-indexed policy-editor element set, so a
    guardian with multiple wards edits one at a time: tap a
    `family-ward-item` row to load that ward's policy into the shared
    editor.

    windows (2026-07-10), linux and web (2026-07-11), macos and ios
    (2026-07-12), android (2026-07-15) and tui (2026-07-23) have landed this
    surface (family-safety.md § Implementation status today) — all 7 apps
    now carry it, so callers no longer need to gate on it at all.

    tui renders the same ids as one flat page (`apps/fauna-tui/src/family.rs`)
    over the shared `FamilyClient` in direct Rust, no FFI hop. Its indexed row
    children (`family-ward-handle`, the approval/incoming-transfer buttons)
    are registered `.within(<row-id>, i)`, so `scope="family-ward-item[i]"`
    queries resolve. `family-tab` is a gated sidebar row (revealed by the
    post-auth `fauna.family.status` read), mirroring `admin-tab`.
    """

    # Localized value-select labels (i18n/strings/en.yaml `family:` block).
    # windows' ComboBoxItem.Name matches the LOCALIZED Content text exactly
    # (reference_windows_flaui_select_exact_name), not the raw wire value —
    # these map the wire value to what `driver.select` must be given.
    UNKNOWN_SENDER_LABELS = {
        "allow": "Allow",
        "hold": "Hold for review",
        "reject": "Reject",
    }
    FEED_SOURCES_LABELS = {
        "allow": "Allow",
        "block": "Block",
    }
    # The bridge-DM gate's knob (family-safety.md § The bridge-DM gate). No
    # `reject` arm, deliberately: bridge DMs arrive by pull, so there is no
    # per-sender refusal stage to bounce from and `hold` is the strictest
    # verdict that loses no message.
    UNKNOWN_PEER_DM_LABELS = {
        "allow": "Allow",
        "hold": "Hold for review",
    }
    # Content-floor selects (family-safety.md § Content policy, Slice C) — the four
    # per-category guardian floors, each inherit|collapse|block.
    CONTENT_FLOOR_LABELS = {
        "inherit": "Use my settings",
        "collapse": "Collapse",
        "block": "Block",
    }

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self, timeout: float = 15.0) -> None:
        """Navigate to the family page via the state-protocol nav (the
        established cross-page convention — e.g. ContactsActions.navigate()
        — not a raw FlaUI click on the `family-tab` NavigationViewItem.

        ⚠ CORRECTED 2026-07-19: the previous reason given here — "a gated WinUI
        FooterMenuItem's UIA peer is unreliable to locate directly even once truly
        Visible" — was WRONG, and it propagated the same mis-diagnosis that kept
        `test_admin_tab_visible_for_admin[windows]` red for three sessions. The peer
        is perfectly locatable (measured: `count("family-tab") == 1`); the row is
        simply the last entry in a nav pane taller than the window, so it is below
        the fold until scrolled. `tab_visible` therefore checks reveal via
        `is_nav_tab_revealed` (windows: hang-free `count()>=1`, since scrolling that
        below-fold footer row hung the FlaUI bridge), not a below-fold scroll.
        The state-protocol nav is still the right call here — it is the established
        cross-page convention (e.g. `ContactsActions.navigate()`), independent of
        any UIA quirk."""
        self.driver.navigate_to("family")
        self.driver.wait_for("family-heading", timeout=timeout)

    # A generous ceiling, not an expected duration — mirrors
    # AdminActions.BOUNCE_LANDED_BUDGET_S.
    BOUNCE_LANDED_BUDGET_S = 30.0

    def reload(self, timeout: float = 15.0) -> None:
        """Force a real re-fetch of the family page's data.

        On web this must bounce through another route first: the SPA
        navigates with SvelteKit's `goto`, which is a no-op when you are
        already on the target route — so the page's `onMount` fetch never
        re-runs, and anything that changed out-of-band (a heartbeat posted
        directly to the nest, another actor's action) stays invisible no
        matter how many times a poll loop re-navigates here. Same class as
        `AdminActions.navigate_users` (see its docstring) — measured
        directly against `test_family_screen_time_budget_and_usage_readouts
        [web]`, which polled a stale "0 of 120 minutes" readout for its
        whole deadline after a real, nest-confirmed heartbeat. Native
        apps rebuild the page on every nav, so they need no bounce.
        (Platform branch lives here in the action layer, never in a test
        file.)

        The bounce is confirmed against the observable, never timed
        (convention 14): a bounce that has not landed yet turns the second
        navigation right back into the same-route no-op this method exists
        to defeat, silently serving the poll loop stale data for its whole
        deadline."""
        if self.driver.is_web():
            self.driver.set_state({"nav": {"stack": [{"view": "settings"}]}})
            self._await_left_family_route()
        self.navigate(timeout=timeout)

    def _await_left_family_route(self) -> None:
        """Block until the web bounce has actually left the family route.

        Falls through on timeout rather than raising: the caller's own poll
        loop and its own assertion produce a far better diagnostic than a
        bare timeout here."""
        deadline = time.monotonic() + self.BOUNCE_LANDED_BUDGET_S
        while time.monotonic() < deadline:  # deadline-ok: documented above — the caller's own poll+diagnosis is the real check
            try:
                nav = self.driver.get_state("nav") or {}
                stack = nav.get("stack") or []
                if stack and stack[0].get("view") != "family":
                    return
            except Exception:
                # A read that fails mid-navigation is not a verdict — retry.
                pass
            time.sleep(0.1)

    def tab_visible(self) -> bool:
        """Whether the gated `family-tab` is reachable from wherever the app
        currently is — the direct visibility check `test_family_transfer_
        {accept,decline}_journey` uses to prove the widened family-tab gate
        (any relationship OR a pending/incoming transfer).

        On desktop (macOS/windows/linux) `family-tab` is a persistent sidebar/
        top-nav entry, so it is either in the tree or it isn't. On mobile
        (iOS/android) it is nested inside Settings (the same shape as
        `admin-tab` — see `test_admin_nav.py`'s module comment, "reached via
        Settings, never on the primary view"), so it is only observable once
        Settings is actually mounted — this navigates there first. Web's
        `family-tab` sits on the persistent top nav bar (unlike its
        Settings-nested `admin-tab`), so it needs no extra navigation either.
        """
        if self.driver.is_mobile():
            self.driver.navigate_to("settings")
        # `is_nav_tab_revealed`, the reveal-is-tree-membership check for a gated nav
        # tab: on a nav pane taller than the window the gated row renders correctly
        # but below the fold, so a bare `is_visible` reads that identically to "the
        # gate never fired". The base impl best-effort-scrolls first; the windows
        # driver overrides to a hang-free `count()>=1` (the family-tab is
        # Collapsed-until-revealed, so tree-membership is an exact + scroll-free
        # reveal signal — scrolling the below-fold footer row hung the FlaUI bridge). Clients where the tab is already in view are unaffected.
        return self.driver.is_nav_tab_revealed("family-tab")

    # --- Guardian section: wards ---

    def ward_count(self) -> int:
        return self.driver.count("family-ward-item")

    def ward_handles(self) -> list[str]:
        count = self.ward_count()
        return [self.driver.get_text("family-ward-handle", index=i) for i in range(count)]

    def select_ward(self, index: int = 0) -> None:
        """Tap a `family-ward-item` row — loads its policy into the shared editor."""
        self.driver.click("family-ward-item", index=index)
        time.sleep(0.5)

    def select_ward_by_handle(self, handle: str, timeout: float = 10.0) -> None:
        """Select the ward row showing `handle`. Callers routinely `reload()`
        first, and the reload refetches the ward list asynchronously — read at
        once, the list can still be empty — so this waits (deadline poll,
        convention 14) for the handle to appear before indexing."""
        deadline = time.monotonic() + timeout
        handles = self.ward_handles()
        while handle not in handles and time.monotonic() < deadline:
            time.sleep(0.5)
            handles = self.ward_handles()
        if handle not in handles:
            raise AssertionError(f"ward {handle!r} not in the guardian's list {handles!r}")
        self.select_ward(handles.index(handle))

    def ward_content_notices_text(self, index: int = 0) -> str:
        """The guardian's per-ward Guardian Notify readout text
        (`family-ward-content-notices`, family-safety.md § Guardian Notify) — the
        day's category + count aggregates for a ward, or "" if none rendered. The
        readout is rendered only for wards that have notices today, so `index` is
        the position among those wards (0 for the single-ward reference journey)."""
        if self.driver.count("family-ward-content-notices") <= index:
            return ""
        return self.driver.get_text("family-ward-content-notices", index=index) or ""

    def ward_usage_today_text(self, index: int = 0) -> str:
        """The guardian's per-ward screen-time readout (`family-ward-usage-today`,
        family-safety.md § Screen time) — the day's cross-device minutes for a
        ward, or "" if none rendered.

        Rendered ONLY while that ward has a daily budget set (no accounting
        without a declared policy), so "" is the meaningful negative here, not a
        missing element. Like the Notify readout, `index` is the position among
        the wards that render one."""
        if self.driver.count("family-ward-usage-today") <= index:
            return ""
        return self.driver.get_text("family-ward-usage-today", index=index) or ""

    # --- The ONE shared reach-policy editor (for the selected ward) ---

    def policy_editor_visible(self, timeout: float = 10.0) -> bool:
        """Whether the reach/content policy editor is loaded for the selected ward.

        Anchored on `family-policy-save-button`, which is the editor's LAST element
        — so every control added above it eats viewport headroom. The v1.x content
        pillar (four content-floor selects + the Notify toggle) pushed it past one
        viewport on windows, whose `is_visible` reads UIA `IsOffscreen`: three
        `test_family.py` journeys then reported "policy editor did not load" against
        an editor that had loaded perfectly, including one that predates content
        policy entirely.

        `is_visible_scrolled` is the honest check here, and it is why that helper
        exists: a bare `is_visible` conflates *"the app never rendered it"* with
        *"it is one scroll away"*. It is cross-app by construction (the scroll is
        best-effort and degrades to a plain `is_visible`) and still returns False for
        a genuinely absent element, so this neither weakens the assertion nor
        introduces a per-app branch.

        A deadline poll (convention 14), not one read: the ward click that loads
        the editor lands asynchronously, and a single read 0.5 s later reported
        "did not load" on windows beside a diagnostic showing the editor's own
        selects visible. Every caller asserts the positive, so polling it cannot
        mask an editor that never loads.
        """
        deadline = time.monotonic() + timeout
        while True:
            if self.driver.is_visible_scrolled("family-policy-save-button"):
                return True
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.5)

    def contact_approval_state(self) -> str:
        """"on"/"off", via the uniform toggle-read idiom (get_attr(id, "state"))."""
        return self.driver.get_attr("family-policy-contact-approval-toggle", "state") or ""

    def set_contact_approval(self, enabled: bool) -> None:
        if (self.contact_approval_state() == "on") != enabled:
            self.driver.click("family-policy-contact-approval-toggle")
            time.sleep(0.3)

    def federation_state(self) -> str:
        return self.driver.get_attr("family-policy-federation-toggle", "state") or ""

    def set_federation(self, enabled: bool) -> None:
        if (self.federation_state() == "on") != enabled:
            self.driver.click("family-policy-federation-toggle")
            time.sleep(0.3)

    def unknown_sender_label(self) -> str:
        return self.driver.get_text("family-policy-unknown-sender-select")

    def set_unknown_sender(self, value: str) -> None:
        """`value` is the wire value (allow|hold|reject)."""
        self.driver.select("family-policy-unknown-sender-select",
                           self.UNKNOWN_SENDER_LABELS[value])

    def feed_sources_label(self) -> str:
        return self.driver.get_text("family-policy-feed-sources-select")

    def set_feed_sources(self, value: str) -> None:
        """`value` is the wire value (allow|block)."""
        self.driver.select("family-policy-feed-sources-select",
                           self.FEED_SOURCES_LABELS[value])

    def unknown_peer_dm_label(self) -> str:
        return self.driver.get_text("family-policy-unknown-peer-dm-select")

    def set_unknown_peer_dm(self, value: str) -> None:
        """`value` is the wire value (allow|hold)."""
        self.driver.select("family-policy-unknown-peer-dm-select",
                           self.UNKNOWN_PEER_DM_LABELS[value])

    # --- Content policy (family-safety.md § Content policy, Slice C) ---

    def require_content_render_verdict_supported(self) -> None:
        """No-op — every app with a family surface renders the shared
        content-render-verdict engine (guardian floor + the viewer's own
        threshold both compose through it). Kept as a call-site marker
        (e2e convention 7's action-layer-gate idiom) so a future app that
        genuinely lacks the surface has an obvious place to add its skip.

        Slice C landed on linux/web (2026-07-18 reference legs), android
        (2026-07-18, the same session — `ContentPolicyStore` caches both the
        guardian floor and the viewer's own thresholds), apple (2026-07-22)
        and windows (2026-07-23). tui's own `ContentPolicyState`
        (`apps/fauna-tui/src/content_policy.rs`) wraps the identical shared
        `fauna_core::obligation` engine byte-for-byte with linux's, wired live
        off `fauna.family.status` (`family.rs:1060`) into both the feed
        (`feed/mod.rs:2309`) and conversations render paths — this call used
        to skip tui claiming "no content-label render surface yet", which was
        never true of the verdict engine itself (only ever-so-briefly true of
        the plain `content-label-badge`, landed well before this note was
        last touched). Corrected 2026-08-27 —
        the content-verdict leg of test_family.py now runs (and passes) on
        tui instead of skipping."""

    def content_notice_blocked(self, scope: str) -> bool:
        """Whether ``scope`` (``post-card[i]``) shows a content-policy BLOCK —
        ``content-policy-blocked-notice`` in state ``blocked``.

        Presence alone no longer says it: an app that tags the collapse arm
        paints the same notice in state ``collapsed``, and the viewer's own
        default spam/phishing thresholds collapse with no guardian involved. An
        app that paints no ``state`` attribute paints no collapse notice at all,
        so its notice is a block."""
        if not self.driver.is_visible("content-policy-blocked-notice", scope=scope):
            return False
        state = self.driver.get_attr("content-policy-blocked-notice", "state", scope=scope)
        return state in (None, "blocked")

    def content_notice_reason(self, scope: str) -> str | None:
        """The guardian's reason painted beside ``scope``'s content-policy
        notice (``content-policy-blocked-reason``), or ``None`` when none
        paints — a floor, the viewer's own threshold, an unreadable document
        (``nest-rules.md`` § The shared rule editor → *The reason is the
        member's to read*)."""
        if self.driver.count("content-policy-blocked-reason", scope=scope) == 0:
            return None
        return self.driver.get_text("content-policy-blocked-reason", scope=scope)

    def content_notice_text(self, scope: str) -> str | None:
        """The text of ``scope``'s ``content-policy-blocked-notice`` — the
        app's fixed frame, never the guardian's reason."""
        if self.driver.count("content-policy-blocked-notice", scope=scope) == 0:
            return None
        return self.driver.get_text("content-policy-blocked-notice", scope=scope)

    def screen_hint_text(self) -> str:
        if not self.driver.is_visible("family-policy-screen-hint"):
            return ""
        return self.driver.get_text("family-policy-screen-hint")

    def require_content_hint(self) -> None:
        """Skip unless this app paints the editor's content-category honest
        bound (``family-policy-content-hint``, user-approved 2026-10-16). tui
        leads (2026-10-21), macos/ios (shared FaunaKit) 2026-10-24, web 2026-10-24, linux 2026-10-30,
        windows 2026-11-08; android lifts the id with its family batch."""
        from helpers.app_surface import require_surface

        require_surface(self.driver, "family-policy-content-hint")

    def content_hint_text(self) -> str:
        if not self.driver.is_visible("family-policy-content-hint"):
            return ""
        return self.driver.get_text("family-policy-content-hint")

    # Every app's family page mounts the shared rule editor in
    # `family-policy-rules-section`, where the four content-floor selects stood
    # (nest-rules.md § The shared rule editor (b); windows, the seventh, retired
    # them 2026-11-04). A floor is a category rule at the standard level,
    # written through the editor and saved by its own button.
    _RULE_REASON = "Set by the guardian"

    def _category_rule_row(self, category: str) -> int | None:
        """The row of the category rule on `category`, or None."""
        from i18n.strings import S

        label = getattr(S.moderation.category, category)
        editor = self._rule_editor()
        for row in range(editor.rule_count()):
            if (editor.has_control(row, "rule-editor-item-category-select")
                    and editor.picked(row, "category") == label):
                return row
        return None

    def _rule_editor(self):
        from actions.rule_editor import RuleEditorActions

        return RuleEditorActions(self.driver)

    def content_floor_label(self, category: str) -> str:
        """The floor label the category rule on `category` (one of
        nsfw|spam|phishing|commercial) stands for in the rule editor — `Use my
        settings` when there is none — so callers keep the floor vocabulary."""
        from i18n.strings import S

        editor = self._rule_editor()
        editor.wait_loaded()
        row = self._category_rule_row(category)
        if row is None:
            return self.CONTENT_FLOOR_LABELS["inherit"]
        verdict = editor.picked(row, "verdict")
        return self.CONTENT_FLOOR_LABELS[
            "block" if verdict == S.rule_editor.verdict.block else "collapse"
        ]

    def set_content_floor(self, category: str, value: str) -> None:
        """Set a per-category content floor. `category` is nsfw|spam|phishing|
        commercial; `value` is the wire value inherit|collapse|block.

        This adds, edits or removes the category rule in the rule editor and
        SAVES it at once (the editor's own button) — the policy form's save
        leaves the rules as they are."""
        from i18n.strings import S

        editor = self._rule_editor()
        editor.wait_loaded()
        row = self._category_rule_row(category)
        if value == "inherit":
            if row is not None:
                self.driver.click("rule-editor-item-remove-button",
                                  scope=f"rule-editor-item[{row}]")
        else:
            if row is None:
                row = editor.add_rule()
                editor.pick(row, "category", getattr(S.moderation.category, category))
            editor.pick(row, "verdict", S.rule_editor.verdict.block if value == "block"
                        else S.rule_editor.verdict.collapse)
        # A floor set before the editor carried no reason; every rule needs one.
        for r in range(editor.rule_count()):
            if not self.driver.get_text("rule-editor-item-reason-input",
                                        scope=f"rule-editor-item[{r}]"):
                editor.type_input(r, "reason", self._RULE_REASON)
        deadline = time.monotonic() + editor.WAIT_S
        while not editor.save_enabled() and time.monotonic() < deadline:
            time.sleep(0.2)
        assert editor.save_enabled(), f"the rules cannot be saved: {editor.status()!r}"
        editor.save()
        editor.wait_status(S.rule_editor.saved)

    def content_notify_state(self) -> str:
        return self.driver.get_attr("family-policy-content-notify-toggle", "state") or ""

    def set_content_notify(self, enabled: bool) -> None:
        if (self.content_notify_state() == "on") != enabled:
            self.driver.click("family-policy-content-notify-toggle")
            time.sleep(0.3)

    # --- Screen time (family-safety.md § Screen time, Slice E) ---
    #
    # The usage WINDOW is the hours the ward may use the account (so "no device
    # after 21:00" is the window 07:00-21:00) and may wrap midnight; both bounds
    # are typed HH:MM. The daily budget is whole minutes across every device.
    # An empty field clears that control — which is why these are text inputs.

    def screen_window_start(self) -> str:
        return self.driver.get_text("family-policy-screen-window-start-input") or ""

    def screen_window_end(self) -> str:
        return self.driver.get_text("family-policy-screen-window-end-input") or ""

    def screen_daily_minutes(self) -> str:
        return self.driver.get_text("family-policy-screen-daily-minutes-input") or ""

    def set_screen_window(self, start: str, end: str) -> None:
        """Type both window bounds as `HH:MM`. Empty strings clear the window
        (bounds come in pairs — the nest refuses a half-set one)."""
        self.driver.clear_and_type("family-policy-screen-window-start-input", start)
        self.driver.clear_and_type("family-policy-screen-window-end-input", end)

    def set_screen_daily_minutes(self, minutes: str) -> None:
        self.driver.clear_and_type("family-policy-screen-daily-minutes-input", minutes)

    # --- The ward's lock (a conditionally-present GLOBAL, not a family-page
    # element): rendered on every page EXCEPT the family page, which stays
    # reachable read-only so the ward can always read their own policy.

    def heartbeat(self, minutes: int, *, focused: bool = True) -> None:
        """Advance the ward client's screen-time clock by `minutes` of foreground
        use and run one production heartbeat step (`screen_time_heartbeat` —
        linux `main.rs`, web `#lib/screen-time-e2e.ts`).

        This is testing.md convention 14's fake clock + run_now poke, and it is
        the ONLY honest way to test this pillar: screen time really is a
        function of elapsed time, so a test that waited for a real heartbeat
        would be DEFUNCT (§ point 14), not merely slow. Nothing about the rules
        is faked — the cadence, the accrual cap and the failure re-credit all
        live in shared Rust and are proven at tier_1 in
        `fauna_core::screen_time::tests`; this drives the real
        `fauna.family.usage_report` wire path.

        ``focused=False`` advances the clock with the app out of front or the
        device locked — minutes the ward did not spend, which must bill nothing
        (`family-client-enforcement.md` § Implementation status today, decision
        (b)). It is sent only when false, so an agent that predates the field
        sees the payload it always did; whether an app's agent honours it is the
        ``screen-time-heartbeat-unfocused`` surface (`surfaces.toml`)."""
        payload: dict = {"minutes": minutes}
        if not focused:
            payload["focused"] = False
        self.driver.call_command("screen_time_heartbeat", payload, timeout=20)

    def screen_lock_visible(self) -> bool:
        return not self.driver.is_absent("screen-time-lock")

    def screen_lock_message(self) -> str:
        return self.driver.get_text("screen-time-lock-message") or ""

    # --- The store-consent lock (account-age-band.md § The account age band →
    # the store-law developer duties bullet): a whole-app GLOBAL on the two
    # store-distributed apps, keyed on the store's own signal for the person on
    # this device — never on supervision or the band.

    def inject_play_consent_signal(self, status, approval_date=None) -> None:
        """Stand in for Play Age Signals' answer and run one store-consent round
        (`store_consent_inject` — android `TestAgent.kt`). Only the platform
        read is replaced: the app runs the real shared round
        (`fauna_ffi::store_consent_round`) over it, writes its device-local state
        back and paints the lock from the round's own message — the
        `set_age_claim` precedent. `status` is `"APPROVED"` / `"PENDING"` /
        `"DECLINED"` or None (Play reported no status); `approval_date` is
        `YYYY-MM-DD` or None."""
        self.driver.call_command(
            "store_consent_inject",
            {"play": {"status": status, "approval_date": approval_date}},
            timeout=20,
        )

    def inject_apple_consent_signal(
        self,
        *,
        parental_consent_required: bool = False,
        adult_notification_required: bool = False,
        current_rating: int | None = None,
    ) -> None:
        """The iOS twin of `inject_play_consent_signal` (`store_consent_inject`
        — `FaunaApp.swift`): stand in for DeclaredAgeRange's
        `requiredRegulatoryFeatures` and StoreKit's `AppStore.ageRatingCode`,
        swap the PermissionKit asker for an inert one (the ask button raises no
        system dialog), and run one real shared round. With the ledger empty
        (D-S8), a changed `current_rating` against the install's stored one is
        what makes a change pending — the first read is a silent baseline."""
        self.driver.call_command(
            "store_consent_inject",
            {"apple": {
                "parental_consent_required": parental_consent_required,
                "adult_notification_required": adult_notification_required,
                "current_rating": current_rating,
            }},
            timeout=20,
        )

    def answer_store_consent_ask(self, answer: str) -> None:
        """A PermissionKit answer (`"approved"` / `"declined"`) arriving on the
        device, as `AskCenter.responses(for:)` delivers it (iOS)."""
        self.driver.call_command("store_consent_inject", {"answer": answer}, timeout=20)

    def reset_store_consent_state(self) -> None:
        """Forget the install's device-local `{ consented_through, last_rating }`
        record, so the next round is a first run again (iOS)."""
        self.driver.call_command("store_consent_inject", {"reset_state": True}, timeout=20)

    def store_consent_asks_sent(self) -> int:
        """How many asks the ask button handed to the (inert, injected) asker."""
        return int(self.driver.get_state("store_consent.asks_sent") or 0)

    def ask_store_parent(self) -> None:
        self.driver.click("store-consent-lock-ask-button")

    def clear_store_consent_signal(self) -> None:
        """Drop the injected signal; the next round reads the real store again."""
        self.driver.call_command("store_consent_inject", {"clear": True}, timeout=20)

    def store_consent_lock_visible(self) -> bool:
        return not self.driver.is_absent("store-consent-lock")

    def store_consent_lock_message(self) -> str:
        return self.driver.get_text("store-consent-lock-message") or ""

    def save_policy(self) -> None:
        self.driver.click("family-policy-save-button")
        time.sleep(1.5)

    # --- Approvals queue (NOT ward-scoped — every ward's queue in one read) ---

    def approval_count(self) -> int:
        return self.driver.count("family-approval-item")

    def approval_text(self, index: int = 0) -> str:
        """One queue row's rendered text. Which field a row shows is the shared
        `fauna_core::format::approval_display_text` rule, per kind: a
        `contact_request` shows the nest-joined `peer_handle`, a `mail_hold` or
        `dm_hold` its `peer_address`, everything else its `summary`. A BLANK row
        here means the rule bound the wrong field — the defect windows shipped
        once for `mail_hold` and which `contact_request`/`dm_hold` inherited
        until 2026-08-14."""
        return self.driver.get_text("family-approval-item", index=index)

    def approve(self, index: int = 0) -> None:
        self.driver.click("family-approval-approve-button", index=index)
        time.sleep(1)

    def deny(self, index: int = 0) -> None:
        self.driver.click("family-approval-deny-button", index=index)
        time.sleep(1)

    # --- Contact pre-approval (v1: hex actor id, no handle resolution) ---

    def add_contact(self, actor_id_hex: str) -> None:
        self.driver.clear_and_type("family-contact-add-input", actor_id_hex)
        self.driver.click("family-contact-add-button")
        time.sleep(1)

    # --- Transfer handshake (family-safety.md § Graduation & transfer) ---
    #
    # Initiation is per SELECTED ward (the transfer input lives beside the
    # policy editor, same hex-actor-id convention as contact-add); the pending
    # proposal renders as `family-transfer-pending` with a cancel button. The
    # incoming prompt (`family-incoming-transfer-item`, indexed) renders for
    # the PROPOSED guardian on the same family page — reachable because the
    # `family-tab` gate widens to "any relationship OR pending/incoming
    # transfer".

    def propose_transfer(self, new_guardian_actor_id_hex: str) -> None:
        self.driver.clear_and_type("family-transfer-input", new_guardian_actor_id_hex)
        self.driver.click("family-transfer-button")
        time.sleep(1)

    def transfer_pending_visible(self) -> bool:
        return not self.driver.is_absent("family-transfer-pending")

    def transfer_pending_text(self) -> str:
        return self.driver.get_text("family-transfer-pending")

    def cancel_transfer(self) -> None:
        self.driver.click("family-transfer-cancel-button")
        time.sleep(1)

    def incoming_transfer_count(self) -> int:
        return self.driver.count("family-incoming-transfer-item")

    def incoming_transfer_text(self, index: int = 0) -> str:
        return self.driver.get_text("family-incoming-transfer-item", index=index)

    def accept_incoming_transfer(self, index: int = 0) -> None:
        self.driver.click("family-incoming-transfer-accept-button", index=index)
        time.sleep(1.5)

    def decline_incoming_transfer(self, index: int = 0) -> None:
        self.driver.click("family-incoming-transfer-decline-button", index=index)
        time.sleep(1.5)

    # --- The guardian-enrolled-device marker, guardian side (family-safety.md
    # § Full visibility for young children, Slice F) ---
    #
    # One `family-device-mark-item` per device of the SELECTED ward, each row
    # containing its own `family-device-mark-toggle` (state attribute on|off).
    # The rows live inside the per-ward editor, so the index space belongs to
    # exactly one ward. Two rules the ward-side badge test already follows and
    # these mirror: the toggle is read/clicked **scoped to its row**, never
    # flat (a flat read on a client that paints outside the row returns nothing
    # for both the marked and unmarked device — a false pass on the negative
    # half, which is exactly how tui's ward-side badge shipped broken); and a
    # row is found by its device **label**, never by position, so the nest's
    # device order is not silently part of the contract.

    def device_mark_count(self) -> int:
        return self.driver.count("family-device-mark-item")

    def device_mark_labels(self) -> list[str]:
        """Every device-mark row's text, in row order — each contains that
        device's DISPLAY IDENTITY (its machine-authored label when one rests
        plaintext, else the short device-id form the nest substitutes — ruled
        2026-08-02, family-safety.md § Full visibility for young children;
        never the ward's user-chosen label, which rests sealed under the
        ward's own root)."""
        return [
            self.driver.get_text("family-device-mark-item", index=i)
            for i in range(self.device_mark_count())
        ]

    def device_mark_index_by_label(self, label: str) -> int:
        """Position of the row naming `label` — order-independent, so the test
        never depends on the nest's device sort."""
        rows = self.device_mark_labels()
        for i, text in enumerate(rows):
            if label in (text or ""):
                return i
        raise AssertionError(
            f"no family-device-mark-item names {label!r}; rows read {rows!r}: "
            f"{self.driver.diagnose('family-device-mark-item')}"
        )

    def device_mark_state(self, index: int = 0) -> str:
        """The row's toggle state, read SCOPED to its row (see the note above)."""
        return (
            self.driver.get_attr(
                "family-device-mark-toggle",
                "state",
                scope=f"family-device-mark-item[{index}]",
            )
            or ""
        )

    def set_device_mark(self, index: int, marked: bool) -> None:
        """Flip the row's toggle if it isn't already in the wanted state. The
        mutation is immediate (`fauna.family.device.mark` is its own RPC, not
        batched behind Save) and the page refetches; callers deadline-poll
        `device_mark_state` for the nest-confirmed result rather than sleeping
        a fixed interval (e2e convention 14)."""
        if (self.device_mark_state(index) == "on") != marked:
            self.driver.click(
                "family-device-mark-toggle",
                scope=f"family-device-mark-item[{index}]",
            )

    # --- The un-deny list (family-safety.md § The bridge-DM gate → *The un-deny
    # surface*): one `family-blocked-peer-item` per `block`-verdict peer of the
    # SELECTED ward, each with its own `family-blocked-peer-allow-button`. Lives
    # inside the per-ward editor, so select the ward first.

    def require_blocked_peers_supported(self) -> None:
        """Every app renders the un-deny list, so nothing skips (e2e convention 7 —
        the platform check lives in the action layer, not the test body, and this
        is where a future gap would be declared).

        tui led 2026-08-28; macOS and iOS (one shared FaunaKit `FamilyView`),
        linux, web and android lifted it 2026-09-26, and windows last."""

    def blocked_peer_rows(self) -> list[str]:
        """Every denied peer row's text, in row order. Each CONTAINS its peer id —
        the only name this nest has for an external bridge peer — but is not
        always exactly it: where the row is a container (linux, web) its text
        also carries its own allow button's label. Match a peer by containment,
        as `device_mark_index_by_label` does."""
        return [
            self.driver.get_text("family-blocked-peer-item", index=i) or ""
            for i in range(self.driver.count("family-blocked-peer-item"))
        ]

    def blocked_peer_ids(self, known: set[str]) -> list[str | None]:
        """The peer id each row names, in row order, resolved against the ids
        the caller seeded (`None` for a row naming none of them)."""
        return [
            next((p for p in known if p in row), None)
            for row in self.blocked_peer_rows()
        ]

    def allow_blocked_peer(self, index: int) -> None:
        """Tap the allow button INSIDE row `index` — scoped to that row, never a
        bare global button index, because which peer a row's button addresses is
        exactly the property the un-deny journey proves. The flip is its own
        `approvals_decide` call, not batched behind Save; callers deadline-poll
        the nest for the result."""
        self.driver.click(
            "family-blocked-peer-allow-button",
            scope=f"family-blocked-peer-item[{index}]",
        )

    # --- Feature limits — the guardian host (family-safety.md § App surface →
    # *Feature limits*): one `family-policy-feature-limits-row` per gated feature
    # for the SELECTED ward, directly after the policy save button. The editor
    # itself is `FeaturePolicyEditorActions`, shared with the admin and self
    # hosts. Lives inside the per-ward editor, so select the ward first.

    def wait_for_feature_limits(self, timeout: float = 20.0) -> None:
        """Wait for the section AND its rows — the section label paints with the
        ward, the rows only once the nest's capability set has been read."""
        self.driver.wait_for("family-policy-feature-limits-section", timeout=timeout)
        self.driver.wait_for("family-policy-feature-limits-row", timeout=timeout)

    def feature_limit_row(self, index: int) -> dict[str, str]:
        """One registry member's row, read scoped to that row — several members
        render at once, so a flat read would answer with another row's text."""
        scope = f"family-policy-feature-limits-row[{index}]"
        return {
            "name": self.driver.get_text("family-policy-feature-limits-name", scope=scope),
            "summary": self.driver.get_text(
                "family-policy-feature-limits-summary", scope=scope
            ),
        }

    def feature_limit_names(self) -> list[str]:
        """Every painted row's member name, in row order — one bulk read, so a
        test can tell which registry members the rows carry at all."""
        return self.driver.get_texts("family-policy-feature-limits-name")

    def wait_for_feature_limit_summary(
        self, index: int, expected: str, timeout: float = 20.0
    ) -> dict[str, str]:
        """Poll row `index` until its summary reads `expected` (the section is
        already visible from an earlier read, so its presence proves nothing
        about the re-read a save triggers); return the last-seen row."""
        deadline = time.monotonic() + timeout
        row = self.feature_limit_row(index)
        while time.monotonic() < deadline:
            if row["summary"] == expected:
                return row
            time.sleep(0.2)
            row = self.feature_limit_row(index)
        return row

    def open_feature_limit_editor(self, index: int) -> None:
        """Open the shared editor for row `index` at the GUARDIAN tier, for the
        selected ward."""
        self.driver.click(
            "family-policy-feature-limits-edit-button",
            scope=f"family-policy-feature-limits-row[{index}]",
        )

    # --- Graduation (reveal-then-confirm) ---

    def begin_graduate(self) -> None:
        self.driver.click("family-graduate-button")
        time.sleep(0.3)

    def graduate_confirm_visible(self) -> bool:
        return self.driver.is_visible("family-graduate-confirm-button")

    def graduate_confirm_text(self) -> str:
        return self.driver.get_text("family-graduate-confirm-button")

    def confirm_graduate(self) -> None:
        self.driver.click("family-graduate-confirm-button")
        time.sleep(1.5)

    # --- Supervised section (rendered when the caller is supervised) ---

    def guardian_handle_text(self) -> str:
        return self.driver.get_text("family-guardian-handle")

    def policy_summary_text(self) -> str:
        """The supervised section's read-only policy summary, or "" while the
        section is not rendered — it appears only once the post-navigate
        `fauna.family.status` read resolves, so a caller's deadline poll must be
        able to read "not yet" without the lookup raising (same shape as
        `ward_usage_today_text`)."""
        if self.driver.count("family-policy-summary") == 0:
            return ""
        return self.driver.get_text("family-policy-summary") or ""
