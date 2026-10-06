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

    def require_unknown_peer_dm_select(self) -> None:
        """Skip unless this app renders `family-policy-unknown-peer-dm-select`
        — e2e convention 7, the platform check lives in the action layer.

        The knob's nest half landed 2026-07-17 and the shared catalog
        (`fauna_core::format::unknown_peer_dm_options`/`_label`) has existed as
        long; **tui shipped the select 2026-08-14** as the lead app, **linux
        and web 2026-08-22**, **macOS and iOS 2026-08-27**, **android
        2026-08-28**, **windows 2026-09-07** — the last of the seven."""
        if not (self.driver.is_tui() or self.driver.is_linux() or self.driver.is_web()
                or self.driver.is_macos() or self.driver.is_ios() or self.driver.is_android()
                or self.driver.is_windows()):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="family-policy-unknown-peer-dm-select",
                detail=(
                    "the bridge-DM gate's knob; tui shipped it 2026-08-14, linux "
                    "and web 2026-08-22, macOS and iOS 2026-08-27, android "
                    "2026-08-28, windows 2026-09-07, all over the shared "
                    "unknown_peer_dm_options catalog"
                ),
                tracked="family-safety.md § The bridge-DM gate → App affordance",
            )

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

    def content_floor_label(self, category: str) -> str:
        """The rendered label of a content-floor select (`category` is one of
        nsfw|spam|phishing|commercial)."""
        return self.driver.get_text(f"family-policy-content-{category}-select")

    def set_content_floor(self, category: str, value: str) -> None:
        """Set a per-category content floor. `category` is nsfw|spam|phishing|
        commercial; `value` is the wire value inherit|collapse|block."""
        self.driver.select(f"family-policy-content-{category}-select",
                           self.CONTENT_FLOOR_LABELS[value])

    def require_guardian_notify_client_supported(self) -> None:
        """Skip unless this app has landed Guardian Notify's client half
        (Slice D-client) — the ward-side enforcement counting + report, and
        the guardian's `family-ward-content-notices` readout (e2e convention
        7 — the platform check lives in the action layer, not the test
        body).

        Built on linux + web (2026-07-19), android (2026-08-02), tui
        (2026-08-02 — `apps/fauna-tui/src/content_policy.rs`'s NotifyAccumulator,
        wired in `family.rs:892,981`; this gate excluding tui was stale, not a
        real gap — family-safety.md § Implementation status today's tui row
        confirms Slice C landed the same day), windows (2026-08-11) and apple
        (macOS + iOS together, 2026-08-21;
        `GuardianNotifyCadence` wraps the shared `FfiNotifyAccumulator` UniFFI
        object directly rather than hand-rolling the accumulator a third
        time, since it lands after the 2026-08-11 lift of the state machine
        into shared Rust)."""
        if not (self.driver.is_linux() or self.driver.is_web() or self.driver.is_android()
                or self.driver.is_windows() or self.driver.is_tui()
                or self.driver.is_macos() or self.driver.is_ios()):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="Guardian Notify's client half (ward-side counting + "
                        "the guardian's content_notices readout)",
                detail="built on linux + web + android + tui + windows + "
                       "apple (macOS + iOS); android e2e stays host-emulator-"
                       "gated separately",
                tracked="family-safety.md § Implementation status today "
                        "(Slice D-client)",
            )

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

    def require_screen_time_supported(self) -> None:
        """Skip unless this app has landed the screen-time client half
        (Slice E) — the guardian's three policy inputs and the ward's
        `screen-time-lock` (e2e convention 7 — the platform check lives in the
        action layer, not the test body).

        Built on linux + web (2026-08-01, the reference legs), tui
        (2026-08-01, the lead-app leg), android (2026-08-02, once the
        UniFFI faces landed), apple (macOS + iOS together, 2026-08-22; `ScreenTimeStore` drives the shared
        `FfiUsageHeartbeat` UniFFI object + the `screenLockMessage`/
        `usageTodayLine`/`parseTimeOfDay`/`parseDailyMinutes` free functions,
        writing no policy logic of its own), and windows (2026-08-24, the
        seventh and last app — `ScreenTimeCache` mirrors the same
        no-policy-logic-of-its-own contract)."""
        if not (self.driver.is_linux() or self.driver.is_web() or self.driver.is_tui()
                or self.driver.is_android() or self.driver.is_macos() or self.driver.is_ios()
                or self.driver.is_windows()):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the screen-time client half (the guardian's window/"
                        "budget inputs and the ward's screen-time-lock)",
                detail="built on all 7 apps (linux, web, tui, android, "
                       "macOS, iOS, windows)",
                tracked="family-safety.md § Implementation status today "
                        "(Slice E client half)",
            )

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

    def heartbeat(self, minutes: int) -> None:
        """Advance the ward client's screen-time clock by `minutes` of foreground
        use and run one production heartbeat step (`screen_time_heartbeat` —
        linux `main.rs`, web `$lib/screen-time-e2e.ts`).

        This is testing.md convention 14's fake clock + run_now poke, and it is
        the ONLY honest way to test this pillar: screen time really is a
        function of elapsed time, so a test that waited for a real heartbeat
        would be DEFUNCT (§ point 14), not merely slow. Nothing about the rules
        is faked — the cadence, the accrual cap and the failure re-credit all
        live in shared Rust and are proven at tier_1 in
        `fauna_core::screen_time::tests`; this drives the real
        `fauna.family.usage_report` wire path."""
        self.driver.call_command("screen_time_heartbeat", {"minutes": minutes}, timeout=20)

    def screen_lock_visible(self) -> bool:
        return not self.driver.is_absent("screen-time-lock")

    def screen_lock_message(self) -> str:
        return self.driver.get_text("screen-time-lock-message") or ""

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

    def require_device_mark_control_supported(self) -> None:
        """Skip unless this app has landed the guardian's device-mark control
        (Slice F's guardian half) — the `family-device-mark-item` rows and
        their toggles on the Family page (e2e convention 7: the platform check
        lives in the action layer, not the test body).

        Built on linux + web + tui (2026-08-01), macos + ios (the apple
        Slice-G lift, mirroring the same shared FaunaKit `FamilyView` on both
        targets), and now windows. android's UI joined 2026-08-01 too but
        STAYS SKIPPED here — this test reads `device_mark_state()` via
        `driver.get_attr(...)`, and android's e2e bridge has no
        `/element/attr` route at all yet
        (
        host-gated: needs a device to design the state-carrying mechanism
        empirically). Widening this gate to android before that lands would
        not skip gracefully — `PlatformDriver.get_attr` raises
        `NotImplementedError` with no android override, a hard failure, not a
        clean skip. windows is the LAST app to land the UI (family-safety.md §
        Implementation status today) — android's bridge gap is now the only
        one left. The ward-facing half of Slice F — the badge + the delete
        refusal — is a different surface and is already built on all of
        linux/web/windows/android/tui/apple."""
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
                surface="the guardian's device-mark control "
                        "(family-device-mark-item rows + toggles)",
                detail="built on linux + web + tui + apple (macos + ios) + "
                       "windows; android's UI joined 2026-08-01 but stays "
                       "skipped pending the android e2e bridge "
                       "attr/enabled-route",
                tracked="family-safety.md § Implementation status today "
                        "(Slice F, guardian half)",
            )

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
        """Skip unless this app renders the un-deny list (e2e convention 7 — the
        platform check lives in the action layer, not the test body).

        tui led 2026-08-28; macOS and iOS (one shared FaunaKit `FamilyView`),
        linux, web and android lifted it 2026-09-26. windows carries the ids
        only as generated constants — no render."""
        if self.driver.is_windows():
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the guardian's un-deny list (family-blocked-peer-item "
                        "rows + their allow buttons)",
                detail="built on tui, macOS, iOS, linux, web and android; "
                       "windows has the ids as constants only",
                tracked="family-safety.md § Implementation status today "
                        "(the ward-ask / un-deny lifts)",
            )

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

    def require_feature_limits_supported(self) -> None:
        """Skip unless this app renders the guardian's feature-limits rows (e2e
        convention 7). tui is the lead app (2026-10-04); the other six follow in
        one batched trickle-down."""
        if not self.driver.is_tui():
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="family-policy-feature-limits-section",
                detail="the guardian host of the shared feature-policy editor; "
                       "its logic is the shared fauna_client_features guardian "
                       "arm behind UniFFI + wasm faces, so the per-app work is "
                       "painting the rows and forwarding keystrokes",
                tracked="",
            )

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
