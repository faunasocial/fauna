from __future__ import annotations

import time
from typing import TYPE_CHECKING

from actions.connected_apps import ConnectedAppsActions
from helpers.budgets import RPC_ROUNDTRIP_S

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class AtprotoSettingsActions:
    """Drive the Bluesky/ATProto login-plane settings page
    (docs/goal/behavior/atproto-pds-full.md § App surface).

    F1 scope: app credentials (mint/reveal/revoke) + connected-app sessions
    (revoke) + the per-account external-apps kill-switch. Mint collects no
    label/dm_allowed input in F1 (no such IDs were approved) — the client
    auto-labels and defaults dm_allowed to false. Plus, since F4 slice 6c,
    the **OAuth consent approval card** (`atproto-consent-*`) at the bottom.
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        """Navigate to the atproto page (the Settings rail
        sub-page). On single-scroll clients the sub-id is ignored and the
        IDs are already on the one Settings surface — the cross-app-safe
        two-element nav the mail/admin actions use."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "atproto"}]},
        })

    def is_page_visible(self, timeout: float = 10.0) -> bool:
        # The page landmark is universal (all apps, every level). The mint
        # button is NOT: on a client with the depth selector it renders only at
        # level = hosted_full, so it can't stand in for "the page rendered".
        # `timeout` defaults to a plain UI wait; a caller checking the FIRST
        # navigation right after a fresh driver launch should pass a more
        # generous budget (e.g. `helpers.budgets.APP_RELAUNCH_S`) — web's boot
        # sequence has a documented, self-limiting double-mount that a plain
        # 10s can lose the race against under real load.
        try:
            self.driver.wait_for("atproto-page", timeout=timeout)
            return True
        except TimeoutError:
            return False

    def has_depth_selector(self) -> bool:
        """Whether this client has the integration-depth selector (it lands
        per-app — ui/atproto.md § Migration). Clients without it render the
        full-PDS surface unconditionally."""
        return self.driver.is_visible("atproto-depth-selector")

    def mint_credential(self) -> None:
        """Mint a new app credential (auto-labeled; F1 has no label input)."""
        self.driver.wait_for("atproto-app-credential-mint", timeout=10.0)
        self.driver.click("atproto-app-credential-mint")

    def credential_count(self) -> int:
        """How many rows the app-credentials list currently renders."""
        return self.driver.count("atproto-app-credential-item")

    def wait_for_credential_count(self, expected: int, timeout: float = 10.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.credential_count() == expected:
                return True
            time.sleep(0.3)
        return self.credential_count() == expected

    def reveal_credential_secret(self, index: int = 0, timeout: float = 8.0) -> str:
        """Reveal (or read an already-revealed) secret on row `index`.

        `atproto-app-credential-reveal`'s own text carries the secret once
        revealed (get_text contract — no separate secret-display ID in F1):
        before reveal it reads the localized "Reveal" label; after, the raw
        secret. If the row is already revealed (e.g. right after mint), the
        button is disabled and its text is already the secret, so this
        returns immediately without clicking.
        """
        scope = f"atproto-app-credential-item[{index}]"
        if not self.driver.is_enabled("atproto-app-credential-reveal", scope=scope):
            # Already revealed (e.g. right after mint, which shows the secret
            # inline per rule 1) — disabled means don't click, just read it.
            return self.driver.get_text("atproto-app-credential-reveal", scope=scope).strip()
        before = self.driver.get_text("atproto-app-credential-reveal", scope=scope).strip()
        self.driver.click("atproto-app-credential-reveal", scope=scope)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            s = self.driver.get_text("atproto-app-credential-reveal", scope=scope).strip()
            if s and s != before:
                return s
            time.sleep(0.25)
        return self.driver.get_text("atproto-app-credential-reveal", scope=scope).strip()

    def revoke_credential(self, index: int = 0) -> None:
        self.driver.click("atproto-app-credential-revoke", index)

    # `atproto-external-apps-enable`'s own on/off, carried via the `state`
    # attr — same idiom as mail-settings-enabled-toggle.
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
        """Drive the kill-switch to `enabled`, idempotently."""
        self.driver.wait_for(self.EXTERNAL_APPS_TOGGLE_ID, timeout=10.0)
        want = "on" if enabled else "off"
        if self.external_apps_state() != want:
            self.driver.click(self.EXTERNAL_APPS_TOGGLE_ID)

    def page_error_text(self, timeout: float = 5.0) -> str:
        """Return the page's `error-message` text once it appears, or "" on
        timeout."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.is_visible("error-message"):
                return self.driver.get_text("error-message")
            time.sleep(0.3)
        return ""

    # ── Integration-depth selector (docs/goal/ui/atproto.md § Layout & flow) ──
    #
    # The four levels' wire spellings; the element ids use hyphens
    # (`hosted_visible` → `atproto-depth-hosted-visible`).
    LEVELS = ("off", "linked", "hosted_visible", "hosted_full")

    @staticmethod
    def _depth_id(level: str) -> str:
        return f"atproto-depth-{level.replace('_', '-')}"

    def depth_level(self) -> str:
        """The selector's current level, carried in `atproto-depth-selector`'s
        `state` attr (the machine's wire spelling)."""
        return self.driver.get_attr("atproto-depth-selector", "state") or ""

    def wait_for_depth_level(self, expected: str, timeout: float = 15.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.depth_level() == expected:
                return True
            time.sleep(0.3)
        return self.depth_level() == expected

    def select_depth(self, level: str) -> None:
        """Click a level rung. An effect-free move (Off→Linked) applies at once;
        every other move stages the transition card for an explicit confirm."""
        assert level in self.LEVELS, f"unknown level {level!r}"
        self.driver.click(self._depth_id(level))

    def is_depth_enabled(self, level: str) -> bool:
        """Whether a rung is selectable — the hosted rungs are disabled (greyed)
        on a non-public domain (§ Reveal/greying rules)."""
        return self.driver.is_enabled(self._depth_id(level))

    def wait_for_depth_enabled(self, level: str, enabled: bool = True, timeout: float = 10.0) -> bool:
        """Wait until a rung reaches `enabled`. The hosted rungs flip to enabled
        only after the first `get_integration_status` refresh reports
        `hosted_allowed` (the snapshot defaults to greyed until then)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.is_depth_enabled(level) == enabled:
                return True
            time.sleep(0.3)
        return self.is_depth_enabled(level) == enabled

    def depth_gate_marker(self, level: str) -> str:
        """`gated` when a hosted rung is greyed with a reason, `ok` otherwise
        (the visible reason line carries the full localized text)."""
        return self.driver.get_attr(self._depth_id(level), "reason") or ""

    # ── The transition card (atproto-depth-confirm-card) ──
    def is_card_visible(self) -> bool:
        return not self.driver.is_absent("atproto-depth-confirm-card")

    def wait_for_card(self, timeout: float = 8.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.is_card_visible():
                return True
            time.sleep(0.25)
        return self.is_card_visible()

    def wait_for_no_card(self, timeout: float = 8.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self.is_card_visible():
                return True
            time.sleep(0.25)
        return not self.is_card_visible()

    def card_text(self) -> str:
        """The composed effect lines, rendered verbatim from the machine's
        `pending_transition.lines`."""
        return self.driver.get_text("atproto-depth-confirm-card")

    def confirm_transition(self) -> None:
        self.driver.click("atproto-depth-confirm")

    def cancel_transition(self) -> None:
        self.driver.click("atproto-depth-cancel")

    def select_did_method(self, method: str) -> None:
        """Choose the mint's DID method (pre-mint only). `plc` | `web`."""
        self.driver.click(f"atproto-did-method-{method}")

    # ── The recovery-fork contest ceremony (atproto-contest-*) ──
    #
    # The remedy for a nest that seized the identity, so every gesture here runs
    # on the client's OWN connection to the public PLC directory — no nest call
    # (`ui/atproto.md` § User actions).
    def is_contest_card_visible(self) -> bool:
        return self.driver.is_visible("atproto-contest-card")

    def contest_state(self) -> str:
        """`contestable` | `window-closed` | `not-contestable` — the three
        honest states, assertable rather than inferred from which controls
        render."""
        return self.driver.get_attr("atproto-contest-card", "state") or ""

    def contest_detail(self) -> str:
        """What the box-authored op did, machine-composed. Names what undoing
        does NOT restore (the nest keeps its publishing key)."""
        return self.driver.get_text("atproto-contest-detail")

    def contest_deadline(self) -> str:
        """The advisory countdown; absent when the directory published no
        parseable timestamp, or the state is terminal."""
        return self.driver.get_text("atproto-contest-deadline")

    def open_contest(self) -> None:
        """`atproto-contest` — open the confirm card. Signs nothing."""
        self.driver.click("atproto-contest")

    def is_contest_confirm_visible(self) -> bool:
        return self.driver.is_visible("atproto-contest-confirm-card")

    def contest_confirm_text(self) -> str:
        return self.driver.get_text("atproto-contest-confirm-card")

    def confirm_contest(self) -> None:
        """`atproto-contest-confirm` — record the scoped intent and submit the
        recovery fork."""
        self.driver.click("atproto-contest-confirm")

    def cancel_contest(self) -> None:
        """`atproto-contest-cancel` — nothing signed, no intent recorded."""
        self.driver.click("atproto-contest-cancel")

    # ── Linked-account panel (level = linked) ──
    def is_linked_panel_visible(self) -> bool:
        """The Linked-account panel is the consume-side link surface reused
        verbatim from the Bridges page — the shared `bridge-link-form` /
        `bridge-card` components (ui/atproto.md § Layout & flow). It adds no
        IDs of its own, so its presence is read off the dual-purpose
        `bridge-action-button` those components carry."""
        return not self.driver.is_absent("bridge-action-button")

    def wait_for_linked_panel(self, visible: bool = True, timeout: float = 8.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.is_linked_panel_visible() == visible:
                return True
            time.sleep(0.25)
        return self.is_linked_panel_visible() == visible

    # ── Hosted panel + delete + full-PDS gating ──
    def hosted_handle_text(self) -> str:
        return self.driver.get_text("atproto-hosted-handle")

    def is_hosted_handle_visible(self) -> bool:
        return not self.driver.is_absent("atproto-hosted-handle")

    def is_did_method_visible(self) -> bool:
        return self.driver.is_visible("atproto-did-method")

    def is_delete_presence_visible(self) -> bool:
        return self.driver.is_visible("atproto-delete-presence")

    # ── The delete ceremony (atproto-delete-confirm-card / -confirm / -cancel) ──
    #
    # Its OWN card, deliberately distinct from the depth selector's transition
    # card above (`ui/atproto.md` § User actions row 4): the delete is not one
    # of the selector's transitions, and a user reading a level-change
    # description must never be the one who confirms a sweep.
    def open_delete_confirm(self) -> None:
        self.driver.click("atproto-delete-presence")

    def is_delete_card_visible(self) -> bool:
        return not self.driver.is_absent("atproto-delete-confirm-card")

    def wait_for_delete_card(self, timeout: float = 8.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.is_delete_card_visible():
                return True
            time.sleep(0.25)
        return self.is_delete_card_visible()

    def wait_for_no_delete_card(self, timeout: float = 8.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self.is_delete_card_visible():
                return True
            time.sleep(0.25)
        return not self.is_delete_card_visible()

    def delete_card_text(self) -> str:
        """The ceremony's copy, rendered verbatim from the machine's
        `delete_confirm.lines` — never re-authored per app."""
        return self.driver.get_text("atproto-delete-confirm-card")

    def confirm_delete(self) -> None:
        self.driver.click("atproto-delete-confirm")

    def cancel_delete(self) -> None:
        self.driver.click("atproto-delete-cancel")

    # ── The ceremony's terminal opt-in (atproto-delete-tombstone, S5 slice 5b) ──
    #
    # Its `state` attr is `on` / `off` / `unavailable` — the last for an
    # identity that cannot be retired (did:web, or a did:plc not yet
    # published), rendered greyed with the reason beside it.
    RETIRE_TOGGLE_ID = "atproto-delete-tombstone"

    def retire_identity_state(self) -> str:
        return self.driver.get_attr(self.RETIRE_TOGGLE_ID, "state") or ""

    def is_retire_identity_visible(self) -> bool:
        return not self.driver.is_absent(self.RETIRE_TOGGLE_ID)

    def toggle_retire_identity(self) -> None:
        self.driver.click(self.RETIRE_TOGGLE_ID)

    def wait_for_retire_identity_state(self, expected: str, timeout: float = 8.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.retire_identity_state() == expected:
                return True
            time.sleep(0.25)
        return self.retire_identity_state() == expected

    def wait_for_no_delete_presence(self, timeout: float = 8.0) -> bool:
        """The button withdraws once the presence is destroyed — its only
        remaining outcome would be a no-op."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self.is_delete_presence_visible():
                return True
            time.sleep(0.25)
        return not self.is_delete_presence_visible()

    def is_fullpds_visible(self) -> bool:
        """The full-PDS panel (mint button / kill-switch) renders only at
        level = hosted_full."""
        return not self.driver.is_absent("atproto-app-credential-mint")

    def wait_for_fullpds_visible(self, timeout: float = 8.0) -> bool:
        """Deadline-polled twin of `is_fullpds_visible` for the "must render"
        assertions — the level flip (`ensure_hosted_full`'s own
        `wait_for_depth_level` poll) and the full-PDS panel's own render are
        two independent elements settling on the same navigate, so a caller
        that checks this one instantaneously right after the other has
        confirmed can race a still-settling page (convention 14: a positive
        wait is a named deadline poll, never a bare instantaneous read). The
        "must NOT render" call sites stay on the bare instantaneous check —
        rule 14's negative asserts anchor to a causal barrier, never a wait."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.is_fullpds_visible():
                return True
            time.sleep(0.25)
        return self.is_fullpds_visible()

    # ── The D10 authoring-delegation row (atproto-pds-full.md § App surface) ──
    #
    # What authorizes an external ATProto app to *post* as the account. The row
    # and its three leaves are absent until a delegation is provisioned, and
    # absent — never rendered as a grant — when the stored cert fails the
    # client-side verify under the account's own identity key. `-authorize`
    # renders in both states (re-minting IS renewal, so it doubles as renew);
    # `-revoke` only alongside a live row.
    def is_delegation_row_visible(self) -> bool:
        return not self.driver.is_absent("atproto-delegation-row")

    def delegation_status(self) -> str:
        """The liveness wire spelling (`active` | `expiring_soon` | `expired` |
        `never_expires`), read off `atproto-delegation-status`'s `state` attr —
        the state itself, not its localized prose."""
        return self.driver.get_attr("atproto-delegation-status", "state") or ""

    def delegation_scope_text(self) -> str:
        return self.driver.get_text("atproto-delegation-scope")

    def delegation_lasts_until_text(self) -> str:
        return self.driver.get_text("atproto-delegation-lasts-until")

    def wait_for_delegation(
        self, present: bool = True, timeout: float = RPC_ROUNDTRIP_S
    ) -> bool:
        """Wait for the delegation row to appear/disappear. Named generous
        budget: a grant is a three-call ceremony (fetch the sub-key →
        identity-sign the cert → provision), so a green run pays nothing and a
        loaded machine still gets a trustworthy verdict (convention 14)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.is_delegation_row_visible() == present:
                return True
            time.sleep(0.3)
        return self.is_delegation_row_visible() == present

    def wait_for_delegation_status(
        self, expected: str, timeout: float = RPC_ROUNDTRIP_S
    ) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.delegation_status() == expected:
                return True
            time.sleep(0.3)
        return self.delegation_status() == expected

    def authorize_delegation(self) -> None:
        """Authorize external apps to post as this account — or re-authorize a
        lapsed grant, which is the same control (provisioning overwrites the
        stored cert, so renewal never requires a revoke first)."""
        self.driver.wait_for("atproto-delegation-authorize", timeout=RPC_ROUNDTRIP_S)
        self.driver.click("atproto-delegation-authorize")

    def current_error_text(self) -> str:
        """Read `error-message` RIGHT NOW — no polling, no wall clock.

        For a NEGATIVE assert ("the gesture did not fail"), polling for a
        window is exactly the settle-sleep convention 14 forbids: it turns a
        real failure that surfaces late into a false GREEN. The causal barrier
        is already available instead — the machine folds a gesture's error and
        its resulting row into the SAME snapshot, so once a post-gesture state
        assertion has passed, this read is authoritative for that same fold.
        """
        if not self.driver.is_visible("error-message"):
            return ""
        return (self.driver.get_text("error-message") or "").strip()

    def revoke_delegation(self) -> None:
        self.driver.wait_for("atproto-delegation-revoke", timeout=RPC_ROUNDTRIP_S)
        self.driver.click("atproto-delegation-revoke")

    def advance_delegation_clock(self, offset_secs: int) -> None:
        """Move the delegation row's RENDER clock (never the mint clock —
        `authorize_delegation` always mints with the real wall clock) by
        `offset_secs` and re-render, so `expiring_soon`/`expired` are
        reachable without waiting out the real ~90-day window (convention 14
        — a fake clock, never a sleep). Pass `0` to reset — the offset is
        process-wide and nothing auto-resets it, so a leftover value would
        silently corrupt the next test's liveness read."""
        self.driver.call_command(
            "atproto_delegation_advance_clock",
            {"now_offset_secs": offset_secs},
            timeout=RPC_ROUNDTRIP_S,
        )

    # ── The OAuth consent approval card (F4 rung 2) ──────────────────────
    #
    # An external ATProto app has started an OAuth sign-in and a browser is
    # blocked waiting for the answer. The card renders only at level =
    # hosted_full and only while a request is live — there is no empty state,
    # so `consent_count() == 0` IS "nothing is waiting".
    #
    # ⚠ The card lives on the Connected apps page on all 7 apps
    # (`ui/connected-apps.md` — a lift, never a duplication), so every helper
    # below reads and answers it on that page.
    def _lifted(self) -> ConnectedAppsActions:
        return ConnectedAppsActions(self.driver)

    def consent_count(self) -> int:
        return self._lifted().request_count()

    def wait_for_consent_count(
        self, expected: int, timeout: float = RPC_ROUNDTRIP_S
    ) -> bool:
        """Wait for the card list to reach `expected`.

        Named generous budget, deadline-polled: opening a consent is a browser
        →bridge→nest round trip plus the app's own re-list, so a green run pays
        nothing and a loaded machine still gets a trustworthy verdict
        (convention 14). NEVER a settle-sleep.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.consent_count() == expected:
                return True
            time.sleep(0.3)
        return self.consent_count() == expected

    def consent_card_text(self, index: int = 0) -> str:
        """The card's own text: who is asking (resolved name + the client_id
        verbatim) and one line per requested scope."""
        return self._lifted().request_text(index)

    def consent_code(self, index: int = 0) -> str:
        """The binding code itself — read off the element's `code` attr, not its
        prose.

        The rendered label is localized ("Confirmation code: …") and rewordable;
        the code is not, and the code is what the user compares against their
        browser. Same discipline as `delegation_status` reading a `state` attr
        rather than its sentence. Scoped inside its own card, so several live
        requests cannot be confused for one another.
        """
        return self._lifted().request_code(index)

    def consent_index_for_code(self, code: str) -> int | None:
        """Which card is showing `code`, or None.

        Tests locate their OWN request this way rather than assuming index 0:
        the list legitimately holds several live requests at once — every
        unassigned one on the nest is listed to every account — so a positional
        assumption makes test order load-bearing and, worse, could approve
        somebody else's sign-in.
        """
        for i in range(self.consent_count()):
            if self.consent_code(i) == code:
                return i
        return None

    def wait_for_consent_code(
        self, code: str, timeout: float = RPC_ROUNDTRIP_S
    ) -> int | None:
        """Wait for a card showing `code` and return its index. Named generous
        budget, deadline-polled (convention 14) — opening a consent is a
        browser→bridge→nest round trip plus the app's own re-list."""
        ca = self._lifted()
        ca.navigate()
        return ca.wait_for_request_code(code, timeout)

    def approve_consent(self, index: int = 0) -> None:
        self._lifted().approve_request(index)

    def deny_consent(self, index: int = 0) -> None:
        self._lifted().decline_request(index)

    def ensure_hosted_full(self, timeout: float = 20.0) -> None:
        """Drive the selector up to hosted_full, confirming the composed card
        (which mints the identity intent), so the full-PDS panel reveals.
        Idempotent. Requires a public-domain nest — the hosted rungs are greyed
        on localhost.

        A no-op on clients without the depth selector: they render the full-PDS
        surface unconditionally (the selector lands per-app — ui/atproto.md
        § Migration), so the full-PDS controls are already present."""
        self.navigate()
        assert self.is_page_visible(), f"atproto page not reachable. error: {self.driver.error_text()!r}"
        if not self.has_depth_selector():
            return
        # A caller may have arranged the level over the wire as a fixture
        # precondition (e2e rule 8) rather than through this same UI flow —
        # give the client's own post-navigate hydrate a chance to catch up
        # before deciding a real selection is owed. A bare single read races
        # a fast-starting client's async refresh: a warm web re-run (no
        # compile step, page interactive almost immediately) lost this race
        # every time, while a cold linux run — with its own build ahead of
        # the check, giving the async refresh time to land in the
        # background — never did. Deadline-polled (convention 14), never a
        # settle-sleep; a green run still returns on its first poll.
        if self.wait_for_depth_level("hosted_full", timeout=RPC_ROUNDTRIP_S):
            return
        assert self.is_depth_enabled("hosted_full"), (
            "hosted_full must be selectable on a public-domain nest; "
            f"gate marker={self.depth_gate_marker('hosted_full')!r}"
        )
        self.select_depth("hosted_full")
        if self.wait_for_card(timeout=6.0):
            self.confirm_transition()
        assert self.wait_for_depth_level("hosted_full", timeout=timeout), (
            f"could not reach hosted_full. level={self.depth_level()!r} "
            f"error={self.page_error_text()!r}"
        )
