"""UI actions for the profile **edit form** (text-only profile publish/edit) —
drives the SELF profile page over the ``profile-edit-*`` test IDs.

The profile page (reached via the top-level ``profile-tab``) has a header whose
``profile-edit-button`` opens the ``profile-edit-form`` (a hidden box shown
async after the current profile is fetched — read-modify-write). The form's
``profile-edit-display-name`` / ``profile-edit-bio`` entries + the repeatable
``profile-edit-link-list`` (rows of ``profile-edit-link-{label,url,remove-button}``)
feed ``fauna-client-profile::build_profile`` → ``fauna.profile.set`` on Save. The
header ``profile-handle`` label then re-renders the published ``display_name``
via ``fauna.profile.get`` (async), falling back to handle→actor_id.

Reference impl: linux (lead, ``apps/fauna-linux/src/views/profile/{mod,edit}.rs``).
The other 5 clients lift the same IDs (priority #1), so this layer is
platform-agnostic. profile.md § Where logic lives → Profile publish/edit.
"""

from __future__ import annotations

import time
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class ProfileActions:
    def __init__(self, driver: "PlatformDriver"):
        self.driver = driver

    # --- navigation ---

    def navigate(self) -> None:
        """Open the SELF profile page."""
        self.driver.set_state({"nav": {"stack": [{"view": "profile"}]}})
        self.driver.wait_for("profile-view", timeout=10.0)

    def require_avatar_banner_upload_supported(self) -> None:
        """Skip unless this app builds avatar/banner image upload (e2e
        convention 7 — the platform check lives in the action layer, not the
        test body).

        tui-first (lead app); web, linux, android, macos, ios and windows
        (landed 2026-08-02, the last of the 7 apps) all have it now —
        android e2e stays host-emulator-gated like every other android
        journey, so this gate has no remaining apps to skip."""
        if not (
            self.driver.is_tui()
            or self.driver.is_web()
            or self.driver.is_linux()
            or self.driver.is_android()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_windows()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="avatar/banner image upload",
                detail="all 7 apps have it",
                tracked="n/a",
            )

    def require_other_profile_offers_browse_supported(self) -> None:
        """Skip unless this app renders the OTHER-profile offers-browse
        surface (e2e convention 7 — the platform check lives in the action
        layer, not the test body).

        linux-lead (item 7); web + windows + macos/ios (shared FaunaKit
        ProfileOffersVM + contact-row tap-through, 2026-06-21) + tui (M5
        slice 2, shared offers_list/status_get + offer_status) lifted it
        (same ui.yaml IDs — each app's contact row resolves by the hex
        actor_id); android lifts after (per-app NEXTs)."""
        if not (
            self.driver.is_linux()
            or self.driver.is_web()
            or self.driver.is_windows()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_tui()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the OTHER-profile offers-browse surface",
                detail="6 of 7 apps have it; android lifts after",
                tracked="per-app NEXTs",
            )

    # --- edit form ---

    def open_edit_form(self) -> None:
        """Click the edit button and wait for the form (shown async after the
        current profile is fetched for the read-modify-write base)."""
        self.driver.click("profile-edit-button")
        self.driver.wait_for("profile-edit-form", timeout=10.0)

    def set_display_name(self, name: str) -> None:
        self.driver.clear_and_type("profile-edit-display-name", name)

    def set_bio(self, bio: str) -> None:
        self.driver.clear_and_type("profile-edit-bio", bio)

    def add_link_row(self) -> None:
        """Append an empty link row (indexed ``profile-edit-link-*`` fields)."""
        self.driver.click("profile-edit-link-add-button")

    # --- avatar / banner (staged path, uploaded on Save) ---
    #
    # ``profile-edit-avatar`` / ``profile-edit-banner`` are NOT OS file inputs on
    # every app (tui has no file-chooser concept — ``tui.md`` § Declared
    # platform absences 4). ``set_input_files`` is the cross-platform stage: web
    # drives Playwright's real file input, native/bridge-backed clients (incl.
    # tui) route through the ``compose`` state protocol
    # (``drivers/http_bridge.py``), same mechanism as feed's ``compose-file``.
    # The upload itself happens at Save, not at stage time (`profile.md` §
    # Where logic lives → Field ownership).

    def set_avatar(self, file_path: str) -> None:
        self.driver.set_input_files("profile-edit-avatar", file_path)

    def set_banner(self, file_path: str) -> None:
        self.driver.set_input_files("profile-edit-banner", file_path)

    def remove_avatar(self) -> None:
        self.driver.click("profile-edit-avatar-remove-button")

    def remove_banner(self) -> None:
        self.driver.click("profile-edit-banner-remove-button")

    def avatar_path_text(self) -> str:
        """The staged local path (empty until a picker/state-patch sets one)."""
        return self.driver.get_text("profile-edit-avatar")

    def banner_path_text(self) -> str:
        return self.driver.get_text("profile-edit-banner")

    def expected_staged_path_text(self, path: str) -> str:
        """What ``avatar_path_text()``/``banner_path_text()`` reads back after
        staging ``path`` — the ONE place the picker-shape difference is known
        (e2e convention 7: platform checks live in the action layer, never
        test files). A typed staged-path field (tui, no OS file-chooser)
        echoes the literal path; a real OS `<input type="file">` (web, and any
        future native picker) can only ever report the browser-mandated
        redacted ``C:\\fakepath\\<basename>`` value — `HTMLInputElement.value`'s
        WHATWG-specified restriction on every engine (Chromium/Firefox/WebKit),
        not a bug, and not something automation can bypass even via CDP's
        `set_input_files`."""
        if self.driver.is_web():
            return f"C:\\fakepath\\{Path(path).name}"
        return path

    def save(self) -> None:
        self.driver.click("profile-edit-save-button")

    def wait_for_save(self, timeout: float = 15.0) -> bool:
        """Poll until the Save lands: the form closes on success
        (``Outcome::SaveDone``), or an error surfaces on the page's
        ``error-message`` and the form stays open. Returns whether it
        succeeded (form closed, no error) — callers assert on this plus
        ``error_text()`` so a failure diagnoses itself."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.driver.count("profile-edit-form") == 0:
                return True
            if self.error_text():
                return False
            time.sleep(0.3)
        return self.driver.count("profile-edit-form") == 0

    def cancel(self) -> None:
        self.driver.click("profile-edit-cancel-button")

    # --- header identity ---

    def handle_text(self) -> str:
        """The header identity label (display_name once published, else handle)."""
        return self.driver.get_text("profile-handle")

    def wait_for_handle(self, expected: str, timeout: float = 15.0) -> bool:
        """Poll the header until it renders ``expected``. The post-save header
        refresh is async (build_profile → fauna.profile.set → fauna.profile.get
        → label), so the transition lags the Save click."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.handle_text() == expected:
                return True
            time.sleep(0.3)
        return self.handle_text() == expected

    # --- the private section (another person's Profile, OTHER only) ---
    # `docs/goal/ui/profile.md` § The private section. Every gesture here only
    # STAGES; nothing is written until `save_private`, and the caller polls the
    # effect it expects (convention 14).

    def set_private_nickname(self, text: str) -> None:
        self.driver.clear_and_type("profile-nickname-field", text)

    def set_private_notes(self, text: str) -> None:
        self.driver.clear_and_type("profile-notes-field", text)

    def add_private_label(self, text: str) -> None:
        self.driver.clear_and_type("profile-label-field", text)
        self.driver.click("profile-label-add-button")

    def save_private(self) -> None:
        self.driver.click("profile-private-save-button")

    def private_nickname(self) -> str:
        return self.driver.get_text("profile-nickname-field")

    def private_notes(self) -> str:
        return self.driver.get_text("profile-notes-field")

    def private_labels(self) -> list[str]:
        """The staged-or-saved label chips, in display order."""
        return self.driver.get_texts("profile-label-chip")

    def public_name_text(self) -> str:
        """The header's secondary line — the public name a nickname replaced;
        ``""`` while no nickname heads the page (the element is absent)."""
        if self.driver.is_absent("profile-public-name"):
            return ""
        return self.driver.get_text("profile-public-name")

    def error_text(self) -> str:
        """Safe read of the page ``error-message`` (``""`` when not shown).

        Stripped: the windows page keeps a 1px ``error-message`` mirror whose
        baseline text is a single space so its UIA peer stays realized while the
        InfoBar is collapsed (reference_windows_error_read_via_state); an unstripped
        read would return ``" "`` (truthy) when there is no error. A real message is
        non-whitespace, so stripping only collapses the no-error sentinel."""
        try:
            if self.driver.is_visible("error-message"):
                return self.driver.get_text("error-message").strip()
        except Exception:
            pass
        return ""

    # --- offers section (another actor's profile, subscriber-browse) ---
    #
    # When ``target`` is another actor (reached by tapping their contact row),
    # the profile's Tiers tab hosts the subscriber-browse offers section
    # (``views/profile/offers.rs``): one ``subscription-offer-row`` per offered
    # paid tier, each with a name / status / Subscribe button. The free
    # "followers" tier is the header ``profile-follow-button``'s job, so it is
    # not a per-row offer.

    def open_tiers_tab(self) -> None:
        """Switch the profile inner stack to the Tiers tab (the offers section)."""
        self.driver.click("profile-tiers-tab")

    def offer_count(self) -> int:
        return self.driver.count("subscription-offer-row")

    def offer_name(self, index: int = 0) -> str:
        return self.driver.get_text("subscription-offer-name", index=index)

    def offer_status(self, index: int = 0) -> str:
        return self.driver.get_text("subscription-offer-status", index=index)

    def subscribe_offer(self, index: int = 0) -> None:
        self.driver.click("subscription-offer-subscribe-button", index=index)

    def wait_for_offer(self, name: str, timeout: float = 15.0) -> bool:
        """Poll until an offer row renders ``name`` (the offers read is async —
        ``offers_list`` + ``status_get`` over the tokio runtime)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self._first_offer_name() == name:
                return True
            time.sleep(0.3)
        return self._first_offer_name() == name

    def wait_for_offer_status(self, expected: str, index: int = 0,
                              timeout: float = 15.0) -> bool:
        """Poll until offer row ``index``'s status reads ``expected`` (the
        post-subscribe status flip is async: subscribe → reply → label)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self._offer_status_safe(index) == expected:
                return True
            time.sleep(0.3)
        return self._offer_status_safe(index) == expected

    def wait_for_offer_status_after_external_grant(
        self, expected: str, index: int = 0,
        timeout: float = 90.0, interval: float = 2.0,
    ) -> str:
        """Deadline-poll an offer row's status until it reads ``expected``,
        **re-activating the Tiers tab on every pass**, and return the last
        status seen.

        :meth:`wait_for_offer_status` polls the rendered label alone, which is
        right for a flip the app's own reply drives (subscribe → reply → label).
        It cannot see a flip that happens **elsewhere** — the author's client
        draining the queue — because there is no push kind for a subscribe
        grant, so a subscriber sitting on the page has nothing to re-render
        from. Re-activating the tab is the app-side door for that:
        ``monetization.md`` § Pillar 1 → *The Tiers-tab re-read door* rules that
        every activation of ``profile-tiers-tab`` re-reads the section, on all 7
        apps. So this helper is also the door's cross-app pin — an app that
        regresses to a page-only re-read hangs here on "Pending approval".

        Latency-independent by construction (convention 14): the caller passes a
        generous named budget and a green run pays only the real grant latency.
        Returns the status rather than a bool so a timeout reports what the row
        actually read — a row stuck on "Pending approval" and a row that never
        rendered are different failures.
        """
        deadline = time.time() + timeout
        seen = self._offer_status_safe(index)
        while time.time() < deadline:
            if seen == expected:
                return seen
            time.sleep(interval)  # sleep-ok: poll interval of a deadline loop
            self.open_tiers_tab()
            seen = self._offer_status_safe(index)
        return seen

    def wait_for_offer_count_after_external_change(
        self, expected: int, timeout: float = 30.0, interval: float = 1.0,
    ) -> int:
        """Deadline-poll the offer-row count until it reads ``expected``,
        **re-activating the Tiers tab on every pass**, and return the last count
        seen.

        The same ruled door as
        :meth:`wait_for_offer_status_after_external_grant`, for the cheaper
        external change a headless actor CAN make: the author adding a tier.
        (There is no headless *approve* — committing one needs the
        ``encrypted_upload`` envelope only the author's client can mint — so a
        grant-driven door test needs a second GUI, which is why the pump test
        owns that arm and this one owns the cross-app door.)
        """
        deadline = time.time() + timeout
        seen = self._offer_count_safe()
        while time.time() < deadline:
            if seen == expected:
                return seen
            time.sleep(interval)  # sleep-ok: poll interval of a deadline loop
            self.open_tiers_tab()
            seen = self._offer_count_safe()
        return seen

    def _offer_count_safe(self) -> int:
        try:
            return self.offer_count()
        except Exception:
            return -1

    def _first_offer_name(self) -> str:
        try:
            if self.offer_count() >= 1:
                return self.offer_name(0)
        except Exception:
            pass
        return ""

    def _offer_status_safe(self, index: int = 0) -> str:
        try:
            return self.offer_status(index)
        except Exception:
            return ""

    # --- OTHER-profile header (follow button) + state-protocol nav ---
    #
    # Some clients reach another actor's profile via the nav-stack ``actor_id``
    # (the state-protocol path) rather than only the contacts-row tap-through; and
    # the OTHER header shows ``profile-follow-button`` (follow = subscribe to the
    # free "followers" tier) in place of the SELF ``profile-edit-button``
    # (profile.md § Layout & flow → Another's profile).

    def require_state_protocol_actor_nav_supported(self) -> None:
        """Skip unless this app's nav patch honours a nav-stack ``actor_id``
        (e2e convention 7 — the platform check lives in the action layer, not
        the test body).

        linux, android, tui, web, macos and ios are the wired legs today: their
        nav patch routes an ``actor_id``-carrying entry through the same
        per-target profile opener the real UI uses (web: `web-bridge/agent.js`
        routes to the contacts tap-through's `/app/profile/<id>` route, with
        the shared trim/case-insensitive self-normalization — 2026-08-15) (android:
        `TestAgent.kt::applyNavPatch` → the existing `profile/{actorId}` route,
        `profileNavTarget` mirroring linux's `test_agent::profile_nav_target`
        self-normalization — 2026-07-30; tui: `automation.rs::apply_nav` →
        `open_profile`, with its own `profile_nav_target` port — 2026-08-15,
        which also FIXED tui's missing self-normalization: it routed the id
        but rendered the viewer's own profile in OTHER shape when the id named
        the viewer; macos + ios: `FaunaMacApp.swift`/`FaunaApp.swift`'s nav-patch
        handler → the shared FaunaKit `ProfileView`/`AppState.profileActorId`
        per-target opener (`MacAppState.openProfile` mirrors linux's), through
        the LIFTED shared-Rust `fauna_core::format::profile_nav_target` UniFFI
        door — the first app to consume the shared fn rather than a fourth
        hand-rolled copy — 2026-08-26; windows: `App.xaml.cs`'s nav-patch
        handler → the same `PendingProfileTarget`/`ProfilePage` per-target
        opener real navigation (ContactsPage's tap-through) already used,
        through the same shared `ProfileNavTarget` UniFFI door — 2026-09-06,
        the last of the seven apps). Every app now honours the actor_id.
        """
        if not (
            self.driver.is_linux()
            or self.driver.is_android()
            or self.driver.is_tui()
            or self.driver.is_web()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_windows()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="nav-stack actor_id (state-protocol profile nav)",
                detail=(
                    "the app's nav patch reads only the stack entry's `view`, so "
                    "`navigate_to_actor` opens the SELF profile instead of the "
                    "named actor"
                ),
                tracked="",
            )

    def navigate_to_actor(self, actor_id: str) -> None:
        """Open another actor's profile via the nav-stack ``actor_id`` (the
        state-protocol equivalent of a contact-row tap; used where a client's e2e
        drives nav through the state protocol).

        Gate with :meth:`require_state_protocol_actor_nav_supported` first — an
        app that ignores the ``actor_id`` lands on the SELF profile and still
        satisfies the ``profile-view`` wait below.
        """
        self.driver.set_state({"nav": {"stack": [{"view": "profile", "actor_id": actor_id}]}})
        self.driver.wait_for("profile-view", timeout=10.0)

    def has_offers_section(self) -> bool:
        return self.driver.is_visible("subscription-offers-section")

    def offer_price(self, index: int = 0) -> str:
        return self.driver.get_text("subscription-offer-price", index=index)

    def is_follow_button_visible(self) -> bool:
        return not self.driver.is_absent("profile-follow-button")

    def is_edit_button_visible(self) -> bool:
        return not self.driver.is_absent("profile-edit-button")

    def wait_for_follow_button(self, timeout: float = 10.0) -> bool:
        """Poll until the OTHER-profile follow button is visible (a Collapsed→Visible
        toggle may need a layout pass before the UIA peer realizes)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                if self.is_follow_button_visible():
                    return True
            except Exception:
                pass
            time.sleep(0.3)
        return self.is_follow_button_visible()

    def wait_for_edit_button(self, timeout: float = 10.0) -> bool:
        """Poll until the SELF-profile edit button is visible — the mirror of
        :meth:`wait_for_follow_button` for the ``is_self`` branch."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                if self.is_edit_button_visible():
                    return True
            except Exception:
                pass
            time.sleep(0.3)
        return self.is_edit_button_visible()

    def follow(self) -> None:
        self.driver.click("profile-follow-button")

    def follow_label(self) -> str:
        return self.driver.get_text("profile-follow-button")

    def wait_for_follow_label(self, expected: str, timeout: float = 15.0) -> bool:
        """Poll the follow button until its label flips to ``expected`` (async)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                if self.follow_label() == expected:
                    return True
            except Exception:
                pass
            time.sleep(0.3)
        return self.follow_label() == expected

    # ---- OTHER-profile secondary relationship actions (profile.md § Layout & flow)

    def is_start_dm_button_visible(self) -> bool:
        return self.driver.is_visible("profile-start-dm-button")

    def wait_for_start_dm_button(self, timeout: float = 10.0) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                if self.is_start_dm_button_visible():
                    return True
            except Exception:
                pass
            time.sleep(0.3)
        return self.is_start_dm_button_visible()

    def start_dm(self) -> None:
        """Click the start-DM button — seeds the Conversations new-thread composer
        with this actor and switches to the Conversations page."""
        self.driver.click("profile-start-dm-button")

    def is_block_button_visible(self) -> bool:
        return self.driver.is_visible("profile-block-button")

    def block(self) -> None:
        """Tap the Block⇄Unblock toggle (`profile-block-button`). On a flipped
        client a first tap blocks (label → "Unblock") and a second unblocks (label
        → "Block"); on a block-only-interim client it blocks once (label →
        "Blocked").

        Waits for the toggle to be enabled first: an app that does not yet know
        which edge this open is on (the open-time read still in flight) holds
        the toggle disabled, and on a slow nest that read outlasts the nav."""
        self.driver.wait_until_enabled("profile-block-button", timeout=30.0)
        self.driver.click("profile-block-button")

    def block_label(self) -> str:
        return self.driver.get_text("profile-block-button")

    # --- request contact (the knock from a profile) ---

    def require_request_contact_supported(self) -> None:
        """Skip unless this app renders ``profile-request-contact-button`` —
        convention 7. tui is the lead app (2026-09-25); macOS + iOS (the shared
        FaunaKit ``ProfileView``), linux and web followed 2026-09-26; the
        remaining apps send the same knock only from the Contacts page so far
        (profile.md § Implementation status today)."""
        if not (
            self.driver.is_tui()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_linux()
            or self.driver.is_web()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="profile-request-contact-button",
                detail="the knock is sent only from the Contacts page on this app",
                tracked="profile.md § Implementation status today",
            )

    def request_contact(self) -> None:
        """Tap ``profile-request-contact-button`` — send the knock to the viewed
        actor (label → "Request sent" once the nest accepts it)."""
        self.driver.click("profile-request-contact-button")

    def wait_for_request_contact_label(self, expected: str, timeout: float = 15.0) -> bool:
        """Poll the button until its label reads ``expected`` (async — the knock's
        ``fauna.inbox.send`` round-trip lands, then the label flips)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                if self.driver.get_text("profile-request-contact-button") == expected:
                    return True
            except Exception:
                pass
            time.sleep(0.3)
        return False

    # --- copy the viewed actor's id ---

    def require_actor_id_copy_readback(self) -> None:
        """Skip unless this app's ``profile-actor-id-copy-btn`` reports what it
        copied — convention 7.

        No driver on any app reads the OS clipboard (only windows' can, and only
        on an interactive desktop), so a copy affordance is asserted through the
        ``copied`` attribute it carries once it has fired: written from the same
        value that reached the clipboard, never re-derived (the web-settings
        copy-link buttons' contract, ``actions/web_settings.py``). tui, linux,
        web, macos + ios (the shared FaunaKit ``CopyButton``) and now windows
        (``ProfilePage`` writes the copied string to the button's
        ``AutomationProperties.HelpText``, which the windows driver's
        ``get_attr`` reads for any non-``disabled`` name) carry it, and so does
        android (``ProfileScreen`` writes it to the button's
        ``stateDescription``, which the androidTest bridge's ``/element/attr``
        answers for any non-special name). android e2e stays
        host-emulator-gated like every other android journey, so this gate has
        no remaining apps to skip."""
        if not (
            self.driver.is_tui()
            or self.driver.is_linux()
            or self.driver.is_web()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_windows()
            or self.driver.is_android()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="profile-actor-id-copy-btn's `copied` attribute",
                detail=(
                    "the button copies, but only tui, linux, web, macos, ios, "
                    "windows and android report the value it wrote, so a test "
                    "cannot assert what reached the clipboard"
                ),
                tracked="profile.md § User actions; the cross-app lift",
            )

    def copy_actor_id(self, timeout: float = 15.0) -> str:
        """Click ``profile-actor-id-copy-btn`` and return the actor id it put on
        the clipboard, read from the button's ``copied`` attribute. Raises
        ``AssertionError`` if the button never reports a copy."""
        from helpers.waiting import wait_until

        self.driver.click("profile-actor-id-copy-btn")
        return wait_until(
            lambda: self.driver.get_attr("profile-actor-id-copy-btn", "copied") or None,
            timeout,
            diagnose=lambda: (
                "profile-actor-id-copy-btn never reported what it copied; "
                f"{self.driver.diagnose('profile-actor-id-copy-btn')} "
                f"error={self.error_text()!r}"
            ),
        )

    def wait_for_block_label(self, expected: str, timeout: float = 15.0) -> bool:
        """Poll the block button until its label flips to ``expected`` (async — the
        knocks_block round-trip lands, then the success closure relabels)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                if self.block_label() == expected:
                    return True
            except Exception:
                pass
            time.sleep(0.3)
        return self.block_label() == expected
