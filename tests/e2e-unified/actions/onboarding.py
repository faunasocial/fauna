from __future__ import annotations

import json
import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class OnboardingActions:
    """Actions for the 5-stage onboarding flow.

    All apps (web + native) use the same element IDs from ui.yaml.
    The only web-specific behavior is state-based navigation (the SPA
    needs set_state to reach the onboarding view) and reading secrets
    from web state.
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate_to_status(self) -> None:
        """Wait until the identity choice screen is showing.

        Native apps launch on identity_choice by default; the web SPA's
        root redirect (`/app/` → `/app/onboarding` when there's no stored
        secret) plus the `app` fixture's `driver.reset()` (which sends the
        agent's `__action: 'reset'` patch — see web-bridge/agent.js — that
        clears identity and `goto('/app/onboarding')`) have already landed
        the page on identity_choice by the time tests call this. We just
        wait for the canary button.

        The legacy implementation pushed `nav.stack=[{view: 'status'}]`,
        which agent.js aliases to `/app/settings` — wrong for the new
        flow. Removed; caller is responsible for being on onboarding.
        """
        self.driver.wait_for("create-identity-button", timeout=15)

    def generate_identity(self) -> None:
        """Click 'Create Identity' and wait for the secret key display."""
        self.driver.wait_for("create-identity-button", timeout=15)
        self.driver.click("create-identity-button")
        self.driver.wait_for("secret-key-display", timeout=10)

    def import_key(self, secret_hex: str) -> None:
        """Import an existing secret key via the UI.

        `clear_and_type`, not `type_text`: the wizard keeps its field state
        across a Back, so on a SECOND visit to `identity_import` the field still
        holds the earlier paste and appending produces a 128-char string that
        fails the parser's hex64 guard — the wizard then sits on
        `identity_import` with a localized error while the caller waits for
        `handle-input` and times out with nothing pointing at the cause. The two
        are identical on the first visit, which is why this went unnoticed for
        as long as no test re-entered the screen.
        """
        self.driver.wait_for("import-identity-button", timeout=15)
        self.driver.click("import-identity-button")
        self.driver.wait_for("paste-secret-field")
        self.driver.clear_and_type("paste-secret-field", secret_hex)
        self.driver.click("import-submit-button")
        time.sleep(2)  # sleep-ok: the commit is a durable keystore write with no observable the wizard exposes before the next screen; callers wait on that screen's own element

    def resume_or_import_identity(self, secret_hex: str) -> None:
        """Land on ``handle_entry`` with ``secret_hex`` as the active identity
        after a crash-relaunch, whichever way the client's long-term store came
        back.

        A mid-onboarding crash leaves a TORN store — the identity secret is
        persisted (import writes it first) but no nest binding is written yet —
        and the relaunch surfaces that same state one of two legitimate ways
        (``common.md`` § Client-state recoverability):

          * store WIPED (a native driver's default relaunch hands the process a
            fresh data dir) → the wizard starts at ``identity_choice``; import
            the key here to reach ``handle_entry``.
          * store PRESERVED (web keeps the page's localStorage across an unclean
            kill) → the launch machine already read the identity-only slot and
            resumed the wizard at ``handle_entry``; ``identity_choice`` never
            renders, so re-importing is impossible and unnecessary.

        Races the two surfaces (``is_visible`` never raises) so there is no
        driver-type branch (testing.md § point 3), then leaves the wizard on
        ``handle_entry`` either way.
        """
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if (self.driver.is_visible("import-identity-button")
                    or self.driver.is_visible("handle-input")):
                break
            time.sleep(0.5)
        if self.driver.is_visible("import-identity-button"):
            self.import_key(secret_hex)
        self.driver.wait_for("handle-input", timeout=15)

    # Removed 2026-05-31 (S5 nest_connect deprecation): continue_to_nest_choice,
    # is_on_nest_choice, claim_local_nest, connect_and_register, request_invite.
    # Removed 2026-07-20 (same cleanup, missed the first pass):
    # cancel_pending_invite, current_invite_status. All drove the eliminated
    # nest_choice / nest_connect / nest_register / nest_claim / invite_request_pending
    # manual-URL surfaces (onboarding.md § Pages that do not exist) and had no
    # surviving callers — request-status-display / request-pending-message /
    # request-cancel-button aren't even in ui.yaml's registry anymore. The
    # handle-first flow is driven by go_to_handle_entry + the run_handle_check /
    # wizard_* helpers below; invite_request's own top-row Recheck/Continue
    # (invite-request-recheck-button / invite-request-continue-button) replace
    # the old pending-screen cancel/status affordances.

    # ── The phrase-only identity restore (`onboarding.md` § 1 Identity) ───

    def open_recovery_entry(self) -> None:
        """identity_choice → `recovery_entry` via the restore CTA.

        Deliberately clicks `restore-from-recovery-kit-button` and not the
        adjacent `recover-lost-box-button`: the two sit side by side and mean
        different repairs (a lost IDENTITY vs a lost NEST), which is why the
        goal doc carries a standing warning about their labels.
        """
        self.driver.wait_for("restore-from-recovery-kit-button", timeout=15)
        self.driver.click("restore-from-recovery-kit-button")
        self.driver.wait_for("recovery-entry-phrase-field", timeout=10)

    def restore_from_recovery_kit(
        self, phrase: str, account: str | None = None
    ) -> None:
        """Fill `recovery_entry` and submit the restore ceremony.

        Assumes the caller has landed on the page (:meth:`open_recovery_entry`).
        `account` is the handle including its domain (`alice@fauna.social`) —
        needed only when the payload does not carry one, since the ceremony is
        pre-identity and a handle's `@domain` is the only thing that can locate
        the home nest. Left `None`, the field stays empty and the payload's own
        `handle=` must supply it.

        Does not wait for an outcome: which one is expected is the test's
        assertion, and both a landed restore (`handle-input`) and every refusal
        (`error-message`) are single elements a caller waits on directly.
        """
        self.driver.fill("recovery-entry-phrase-field", phrase)
        if account is not None:
            self.driver.fill("recovery-entry-account-field", account)
        self.driver.click("recovery-entry-submit-button")

    # ── The recovery-kit offer (`onboarding.md` § 1 Identity) ─────────────
    #
    # The screen after `identity_created` on the CREATE path: strongly
    # encouraged, skippable, and it **mints and displays only** — no nest
    # exists at this position, so registration and the escrow put run at the
    # wizard's signed-in handoff (`identity-succession.md` § The RecoveryKey →
    # *Creation UX*, ratified 2026-08-01).

    def recovery_kit_showing(self) -> bool:
        """Whether the wizard is parked on the `recovery_kit` screen.

        `is_visible` never raises for a missing element, so this is safe to
        call on an app whose leg has not landed — which is what lets the
        caller gate on it with `skip_unbuilt` rather than time out.
        """
        return self.driver.is_visible("recovery-kit-secret-display")

    def recovery_kit_secret(self) -> str:
        """The minted 64-hex root the screen shows once, or "".

        Nothing on the device keeps a copy (§ The RecoveryKey — *Custody*), so
        a test that needs it must read it while the screen is up.
        """
        if not self.recovery_kit_showing():
            return ""
        return self.driver.get_text("recovery-kit-secret-display")

    def recovery_kit_escrow_status(self) -> str:
        """The screen's one escrow line, or "".

        It renders exactly one state here — the deferred line — and must never
        imply the account is already protected (`onboarding.md` § 1 Identity).
        """
        if not self.driver.is_visible("recovery-kit-escrow-status"):
            return ""
        return self.driver.get_text("recovery-kit-escrow-status")

    def confirm_recovery_kit(self) -> None:
        """"I've saved it" — keeps the minted root for the signed-in handoff
        and advances to `handle_entry`."""
        self.driver.click("recovery-kit-confirm-button")
        self.driver.wait_for("handle-input", timeout=15)

    def skip_recovery_kit(self) -> None:
        """Decline the kit: one click, never blocks onboarding. The minted root
        is dropped, so nothing registers at the handoff and Settings' standing
        never-created warning tells the truth."""
        self.driver.click("recovery-kit-skip-button")
        self.driver.wait_for("handle-input", timeout=15)

    # -- sign-out residue (account-scoping.md § Erasure follows scope → the
    # residue surface) — what a sign-out's erase could not remove, painted on
    # identity_choice while the install-scoped record owes work.

    def sign_out_residue_showing(self) -> bool:
        """Whether identity_choice is painting the `sign-out-residue` view.

        Read as presence (`is_absent`), never as `is_visible`, so this doubles
        as an exact negative assertion: a clean sweep must paint nothing at
        all, and a view below the fold is still a view (convention 6's rider).
        """
        return not self.driver.is_absent("sign-out-residue")

    def sign_out_residue_text(self) -> str:
        """The residue's one line, or "" when the view is absent."""
        if not self.sign_out_residue_showing():
            return ""
        return self.driver.get_text("sign-out-residue-message")

    def retry_sign_out_residue(self) -> None:
        """Press Remove Again — the re-sweep over exactly what was left. It is
        synchronous app-side; the caller polls the outcome (the view gone, or
        its line replaced)."""
        self.driver.click("sign-out-residue-retry-button")

    def stored_secret(self) -> str:
        """Get the secret key from app state (web only)."""
        if self.driver.is_web():
            state = self.driver.get_state("session.secret_hex")
            return state or ""
        # Native apps don't expose their keychain
        return ""

    # ── Handle-first onboarding helpers ──────────────────────────────────
    #
    # These drive the new wizard (`identity_choice` → `identity_*` →
    # `handle_entry` → (`invite_request` | `claim_code` | `dns_config` →
    # `vps_config` → `nest_provisioning` → (`dns_post_instructions` | Done)))
    # by walking the UI step-by-step. They use the same element IDs across all
    # 6 apps (per ui.yaml), so subclasses don't need overrides — but
    # individual clients can short-circuit (e.g. inject sessionStorage on
    # web) by overriding before calling super().
    #
    # Tests reach a target stage via these helpers, then assert IDs/visibility
    # on that stage. The verified-VPS / verified-DNS helpers drive the real
    # verify path through the `fakes/fake_cloud.py` HTTP stub (web-only until
    # the native drivers implement `set_provider_base_urls` —
    # a fake-cloud provisioning follow-up, Track 2).

    _IMPORT_KEY_FOR_HANDLE_TESTS = (
        # Deterministic 32-byte test key — irrelevant to handle/nest/dns/vps
        # logic, but stable so identity-stage flakes don't bleed into
        # downstream tests.
        "0000000000000000000000000000000000000000000000000000000000000001"
    )

    def go_to_handle_entry(self,
                           secret_hex: str | None = None) -> None:
        """Drive identity_choice → identity_import → handle_entry.

        Default uses a deterministic test secret. Tests that care about
        identity origin (e.g. created-vs-imported back-button targets)
        should call ``generate_identity()`` then continue manually.
        """
        secret = secret_hex or self._IMPORT_KEY_FOR_HANDLE_TESTS
        self.navigate_to_status()
        self.import_key(secret)
        self.driver.wait_for("handle-input", timeout=15)

    # NOTE (no-modes retirement, ratified 2026-07-12): the former
    # encryption_mode_choice page's four deployment-toggle checkboxes
    # (`onboarding-enable-{email,caldav,carddav,webdav}-checkbox`) are RETIRED
    # WITHOUT relocation — mail/CalDAV/CardDAV/WebDAV enablement is now a
    # MACHINE-DERIVED default (ON iff the handle targets a real registerable
    # domain), applied by the post-claim launch glue with no onboarding-time
    # choice (`docs/goal/behavior/onboarding.md` § 3b). There is nothing left
    # to read or set here at onboarding time; to read the settled derived
    # state after claim, or to change it, use the admin Mail / Calendar /
    # Contacts / Files settings pages (`actions/admin.py`
    # `toggle_mail_enabled` / `toggle_caldav_enabled` / `toggle_carddav_enabled`
    # / `toggle_webdav_enabled`) — the real post-onboarding path.

    # ── Provisioning → NAT mode (`nest_provisioning`, onboarding.md § 6) ─────
    #
    # On the standard path `Succeeded` means the box is built AND claimed — the
    # claim is `Online`'s last substep, run by the machine ("Provisioning =
    # build + claim", ratified 2026-08-29) — and the bottom-row Continue lands
    # on `nat_mode_choice` exactly as a claim-code submit does. The in-run
    # claim does NOT move the wizard's step by itself: only Continue does, and
    # `continue_from_provisioning` is app-called (not on the bridge dispatch
    # table), so a flow that provisioned a real box reaches the NAT-mode page
    # the way a user does — by clicking (convention 8).

    _PROVISIONING_CONTINUE_BUTTON = "provisioning-continue-button"

    def continue_from_provisioning(self, *, timeout: float = 30.0) -> None:
        """Click Continue on a ``Succeeded`` `nest_provisioning` page and wait
        for `nat_mode_choice` to show; pair with `finish_nat_mode()` to leave
        the wizard.

        Waits for the button to ENABLE before clicking rather than clicking at
        once: the machine reopens `Online` for its claiming substep after the
        orchestrator has already published `Succeeded`, so a caller that saw
        `overall == Succeeded` in the snapshot may still be inside the claim
        (`can_continue` is false until it lands — a state gate, not a timing
        one). Raises with the button's diagnosis if it never enables (a
        refused claim leaves the run `Failed`, Continue disabled and Retry
        showing), and with the page's if NAT mode never renders after the
        click.
        """
        deadline = time.monotonic() + timeout
        while (time.monotonic() < deadline
               and not self.driver.is_enabled(self._PROVISIONING_CONTINUE_BUTTON)):
            time.sleep(0.5)
        assert self.driver.is_enabled(self._PROVISIONING_CONTINUE_BUTTON), (
            "provisioning Continue never enabled — the run is not (or is no "
            "longer) Succeeded; on the standard path that is a claim that has "
            "not landed (a refused claim fails Online and shows Retry). "
            f"{self.driver.diagnose(self._PROVISIONING_CONTINUE_BUTTON)}"
        )
        self.driver.click(self._PROVISIONING_CONTINUE_BUTTON)
        self.driver.wait_for(self._NAT_CONFIRM_BUTTON, timeout=timeout)

    # ── NAT-mode choice (`nat_mode_choice`, onboarding.md § 3b-bis) ─────────
    #
    # The terminal admin-path wizard step, reached once the claim completes.
    # It pre-selects the nest's seeded `node_mode`, so the common case is
    # confirm-only — but the wizard does NOT reach `Done` (and the LoggedIn
    # launch glue does not fire) until this page is dismissed. Every UI flow
    # that drives an admin claim through to the logged-in app must therefore
    # pass through here.
    #
    # `finish_nat_mode` is deliberately TOLERANT of the page being absent: it
    # returns False instead of raising. Two callers depend on that — a client
    # whose `nat_mode_choice` view hasn't landed yet (the Slice-2 fan-out is
    # per-app), and a flow that already exited the wizard by another route.

    _NAT_PUBLIC_RADIO = "public-nat-mode-radio"
    _NAT_PRIVATE_RADIO = "private-nat-mode-radio"
    _NAT_CONFIRM_BUTTON = "nat-mode-confirm-button"
    _NAT_DEFER_BUTTON = "nat-mode-defer-button"

    def nat_mode_showing(self) -> bool:
        """True iff the wizard is parked on `nat_mode_choice`.

        `is_visible` returns False for a missing element (it never raises), so
        this is safe to call on a client that doesn't render the page.
        """
        return not self.driver.is_absent(self._NAT_CONFIRM_BUTTON)

    _TRUST_SUMMARY = "trust-box-summary"
    _TRUST_GRANT_BUTTON = "trust-box-grant-button"
    _TRUST_SKIP_BUTTON = "trust-box-skip-button"

    def trust_prompt_showing(self) -> bool:
        """True iff the wizard is parked on `trust_prompt` (onboarding.md
        § 3b-ter).

        Like `nat_mode_showing`, safe on a client that doesn't render the page:
        `is_visible` returns False for a missing element.
        """
        return not self.driver.is_absent(self._TRUST_GRANT_BUTTON)

    def finish_trust_prompt(self, *, grant: bool = False, timeout: float = 30.0) -> bool:
        """Answer `trust_prompt` if it is showing; return whether we did.

        `grant=True` taps "trust this box" (the client mints the default grant
        set at the signed-in handoff); the default declines, which per
        § 3b-ter leaves everything as today. Tolerant of the page being
        absent for the same reason `finish_nat_mode` is — a flow that exited
        the wizard another way (every app now declares
        `set_renders_trust_prompt`).
        """
        if not self.trust_prompt_showing():
            return False

        self.driver.click(
            self._TRUST_GRANT_BUTTON if grant else self._TRUST_SKIP_BUTTON
        )
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline and self.trust_prompt_showing():
            time.sleep(0.5)
        assert not self.trust_prompt_showing(), (
            "onboarding did not advance past trust_prompt — both of its "
            "buttons conclude the wizard, so a page still showing means the "
            "click never reached the machine. "
            f"{self.driver.diagnose(self._TRUST_GRANT_BUTTON)}"
        )
        return True

    def finish_joiner_trust_prompt(
        self, *, grant: bool = False, timeout: float = 30.0
    ) -> None:
        """Answer the offer a JOIN route raises (onboarding.md § 3b-ter).

        The claim path's `finish_nat_mode` answers the interstitial for its
        callers; a join — redeeming an out-of-band invite code, or a poll tick
        picking up an approved request — has no such single gateway, so the
        step is named here instead of being open-coded per journey.

        The wait is on the offer APPEARING, never on a stretch of wall clock:
        every app declares `set_renders_trust_prompt`, so a join that concludes
        without raising it is a routing regression, and `wait_for` reports that
        as a missing element rather than as a flake (convention 14 — assert
        latency-independent state).
        """
        self.driver.wait_for(self._TRUST_GRANT_BUTTON, timeout=timeout)
        self.finish_trust_prompt(grant=grant, timeout=timeout)

    def finish_nat_mode(
        self,
        *,
        mode: str | None = None,
        defer: bool = False,
        trust: str | None = "skip",
        timeout: float = 30.0,
    ) -> bool:
        """Dismiss `nat_mode_choice` if it is showing; return whether we did.

        `mode` (``"public"`` / ``"private"``) selects a radio before confirming;
        omit it to accept the pre-selected seed — the confirm-only common case
        the page is designed around. `defer` clicks "Decide later" instead,
        which sends nothing and keeps the seed (also a working default).

        Returns False (no-op) when the page isn't showing, so shared onboarding
        paths can call this unconditionally.
        """
        if not self.nat_mode_showing():
            return False

        if defer:
            self.driver.click(self._NAT_DEFER_BUTTON)
        else:
            if mode is not None:
                radio = {
                    "public": self._NAT_PUBLIC_RADIO,
                    "private": self._NAT_PRIVATE_RADIO,
                }.get(mode)
                if radio is None:
                    raise ValueError(f"unknown NAT mode {mode!r} (want public/private)")
                self.driver.click(radio)
            self.driver.click(self._NAT_CONFIRM_BUTTON)

        # The wizard advanced ⇔ the page is gone. Mirrors the storage-mode
        # helper's disappearance signal (helpers/mail_client_ui.py).
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline and self.nat_mode_showing():
            time.sleep(0.5)
        assert not self.nat_mode_showing(), (
            "onboarding did not advance past nat_mode_choice (the "
            f"fauna.setup.nat_mode commit failed). status: "
            f"{self.driver.get_text('nat-mode-status')!r}"
        )
        # On an app that renders it, leaving this page lands on the one-tap
        # trust offer rather than on `Done` (onboarding.md § 3b-ter). Every
        # caller of this helper wants past the wizard, so the interstitial is
        # answered here by default — pass `trust=None` to leave it on screen
        # for a test that asserts about it.
        if trust is not None:
            if trust not in ("skip", "grant"):
                raise ValueError(f"unknown trust answer {trust!r} (want skip/grant/None)")
            self.finish_trust_prompt(grant=(trust == "grant"), timeout=timeout)
        return True

    def fill_handle(self, handle: str) -> None:
        """Type a handle into ``handle-input`` on the handle_entry stage.

        Assumes the test has already landed on handle_entry (e.g. via
        ``go_to_handle_entry``). Clears any existing value first.
        """
        self.driver.wait_for("handle-input", timeout=10)
        self.driver.clear_and_type("handle-input", handle)

    def submit_handle(self) -> None:
        """Click ``handle-entry-continue-button``.

        Triggers the machine's ``submit_handle_check_continue()`` (after a
        handle check has completed and ``continue_enabled`` is true).
        Caller waits for the next-stage IDs.
        """
        self.driver.click("handle-entry-continue-button")

    def run_handle_check(self, timeout: float = 20) -> None:
        """Click ``handle-check-button`` and wait for the check to complete.

        Post-target-state flow: the user clicks the check button to fire
        ``start_handle_check()``; the machine runs the DoH probe and updates
        ``handle_check_snapshot()``. Completion is detected when the
        ``handle-message-area`` text indicates an outcome has been reached.

        The machine sets ``snapshot.message.key`` to:
          - ``handle_check.phase.*`` during the probe  (in-progress)
          - ``handle_check.outcome.*`` when done        (complete)
          - ``handle_check.error.*`` on errors          (complete)

        The i18n keys may be returned as raw strings if the lookup table
        doesn't have them, OR as resolved English strings (ending in ``…``
        for phase messages). Both forms are handled.
        """
        self.driver.wait_for("handle-check-button", timeout=10)
        self.driver.click("handle-check-button")
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:  # deadline-ok: documented below — a timeout leaves Continue disabled, the caller's own next step
            try:
                msg = self.driver.get_text("handle-message-area").strip()
                if msg:
                    # Raw i18n key form: check if it's an outcome or error key.
                    if (msg.startswith("handle_check.outcome.")
                            or msg.startswith("handle_check.error.")):
                        return  # Outcome/error key — check complete.
                    # Resolved English form: phase messages end in "…" or
                    # "..."; outcome messages don't.
                    if not (msg.endswith("…") or msg.endswith("...")):
                        # Non-phase message: either an outcome or an error.
                        # Also check it doesn't look like a phase key.
                        if not msg.startswith("handle_check.phase."):
                            return
            except Exception:
                pass
            time.sleep(0.5)
        # Timed out — caller will continue but Continue button may be disabled.

    # Removed 2026-05-31 (S5 nest_connect deprecation): go_to_nest_select_with_handle
    # + go_to_nest_select_with_status. nest_select is eliminated (onboarding.md
    # § Pages that do not exist); use go_to_handle_entry + fill_handle +
    # run_handle_check to park on handle_entry, then go_to_dns_config /
    # go_to_invite_request to advance per the handle-check outcome. Neither helper
    # had surviving callers.

    def go_to_dns_config(
        self, handle: str = "alice@not-a-real-fauna-e2e-domain.io",
    ) -> None:
        """Drive identity → handle_entry → handle-check → dns_config.

        ``handle`` defaults to an unregistered name directly under a TLD; a
        name with no delegation inside a held zone (``…example.com``) takes
        the same DomainAvailable path with the inside-zone message.

        Post-target-state: the machine advances directly from handle_entry
        to dns_config when the handle-check outcome is DomainAvailable
        (``continue_enabled=true``).  The user then clicks Continue.

        Uses a non-routable IANA-reserved test domain that the machine's
        DoH probe reliably reports as Unregistered / DomainAvailable.
        """
        # Use a domain with a valid TLD that the handle-check probe will
        # return as DomainAvailable (unregistered). The `.example` TLD is
        # rejected by the probe as TldInvalid. Use `.io` which is in the
        # valid-TLD list and is almost certainly unregistered for this name.
        self.go_to_handle_entry()
        self.fill_handle(handle)
        self.run_handle_check(timeout=20)
        # Give the GTK refresh closure one cycle to apply the new snapshot
        # (continue_btn.set_sensitive) before we try clicking.
        time.sleep(0.5)
        # Verify the check produced a DomainAvailable-class outcome by
        # inspecting handle-message-area text.  DomainAvailable outcomes
        # set continue_enabled=true. TldInvalid, FormatInvalid, and errors
        # do not. Skip if we didn't get a DomainAvailable-class outcome.
        msg = ""
        try:
            msg = self.driver.get_text("handle-message-area").strip()
        except Exception:
            pass
        # Determine if Continue should be enabled. DomainAvailable outcomes
        # surface in one of two client-dependent forms:
        #   raw key (linux/web/windows): handle_check.outcome.domain_available_*
        #     — their i18n lookup misses the `onboarding.`-prefixed key the
        #     machine omits, so `LocalizedText::resolve` falls back to the raw
        #     key (libs/fauna-core/src/localized.rs).
        #   resolved English (macos/ios): `renderLocalizedText` finds the
        #     prefixed template, so the message reads "… appears to be
        #     available …" / "… is available …". Match both. The three
        #     domain_available_* templates are the only outcome/error strings
        #     containing those phrases; the in-progress "Checking domain
        #     availability…" is excluded by `is_in_progress` below.
        # AlreadyOnNest and NestRunningUserUnregistered also enable Continue;
        # we skip on those since we need DomainAvailable for the dns_config path.
        is_domain_available_key = msg.startswith("handle_check.outcome.domain_available")
        is_domain_available_resolved = (
            "appears to be available" in msg or "is available" in msg
            or "It sits inside" in msg  # domain_available_inside_zone
        )
        is_domain_available = is_domain_available_key or is_domain_available_resolved
        is_in_progress = (msg.startswith("handle_check.phase.")
                          or msg.endswith("…") or msg.endswith("...") or not msg)
        if is_in_progress or not is_domain_available:
            from helpers.app_surface import skip_environment

            # Environment, not app: no app work makes a registered domain
            # available, so --strict-app deliberately leaves this a skip.
            skip_environment(
                "handle-check did not return DomainAvailable for the test domain — "
                "likely network-blocked, TLD invalid, or domain is registered. "
                f"message-area text: {msg!r}"
            )
        # continue_enabled is true for DomainAvailable outcomes;
        # click Continue → dns_config.
        self.driver.click("handle-entry-continue-button")
        # Wait on dns-buy-domain-checkbox as the page-entry canary instead
        # of dns-provider-row: AT-SPI on Linux doesn't expose plain
        # gtk::Box descriptions reliably (see test_back_buttons.py for
        # context), so a Box-typed container ID times out even when the
        # page is rendered. The two-checkbox row is unique to dns_config
        # and uses CheckButton, which has a definite accessible role.
        self.driver.wait_for("dns-buy-domain-checkbox", timeout=15)

    def fill_dns_contact(self, contact: dict[str, str]) -> None:
        """Type a WHOIS contact into the dns_config contact form.

        Caller must have already navigated to dns_config and reached
        ``provider_status == UnregisteredBuyable`` on a registrar that
        requires per-registration contact (Gandi today). The form is
        hidden in any other state — calling this then will fail on the
        first ``type_text`` because the entries aren't visible.

        ``contact`` keys map to ContactInfo fields:
        first_name, last_name, email, phone, address1, city, state,
        postal_code, country.
        """
        for field, test_id in [
            ("first_name", "dns-contact-first-name-input"),
            ("last_name", "dns-contact-last-name-input"),
            ("email", "dns-contact-email-input"),
            ("phone", "dns-contact-phone-input"),
            ("address1", "dns-contact-address1-input"),
            ("city", "dns-contact-city-input"),
            ("state", "dns-contact-state-input"),
            ("postal_code", "dns-contact-postal-code-input"),
            ("country", "dns-contact-country-input"),
        ]:
            value = contact.get(field, "")
            self.driver.type_text(test_id, value)

    def go_to_vps_config(self) -> None:
        """Drive identity → ... → dns_config → vps_config (DNS deferred).

        Uses ``dns-set-up-later-button`` so the test doesn't need real
        provider creds. Tests that require a verified DNS provider before
        VPS should use ``go_to_vps_config_with_dns_provider``.
        """
        self.go_to_dns_config()
        self.driver.click("dns-set-up-later-button")
        # Wait on vps-config-back-button as the page-entry canary instead
        # of vps-provider-row: Linux AT-SPI doesn't expose plain gtk::Box
        # descriptions reliably, so a Box-typed container ID times out
        # even when the page is rendered. The back button is unique to
        # vps_config and uses Button, which has a definite accessible role.
        self.driver.wait_for("vps-config-back-button", timeout=15)

    def go_to_vps_config_with_dns_provider(self, provider_id: str,
                                           fake_cloud) -> None:
        """Drive to vps_config with the DNS provider preselected for VPS.

        Navigates to dns_config, then drives the DNS-provider setup through
        the machine bridge: toggles ``same_provider_for_vps``, selects the
        provider, fills creds, runs a real ``verify_dns`` against the
        ``fake_cloud`` stub, then ``continue_from_dns`` — which mirrors the
        provider (creds + verified) onto the VPS stage, so vps_config shows
        it preselected.

        The provider *selection* is driven via ``call_machine_method`` rather
        than UI clicks because the dns_config UI gates provider buttons on
        ``buy_domain``: a DomainAvailable handle sets ``buy_domain=true``,
        which disables every non-registrar provider (Hetzner included), and
        the buy-domain checkbox is itself disabled when ``domain_status !=
        Unregistered`` — so the provider can't be reached by clicking. The
        behavior under test (the continue_from_dns mirror) and the real
        ``verify_dns`` probe are both still exercised.

        Web + linux: both implement ``set_provider_base_urls`` (web via its
        ``?fauna_e2e_provider_base_urls`` channel; linux via the
        ``call_machine_method`` bridge to the runtime setter) plus the
        ``call_machine_method`` DNS-stage dispatch (``select_dns_provider`` /
        ``set_dns_cred`` / the async ``verify_dns`` driven to completion). The
        remaining native apps (apple / windows / android) gain the same
        under a fake-cloud provisioning follow-up, Track 2 (then this guard widens to them).
        """
        if not (self.driver.is_web() or self.driver.is_linux()):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="set_provider_base_urls + the call_machine_method DNS "
                        "dispatch (select_dns_provider / set_dns_cred / verify_dns)",
                detail="fake-cloud provider redirection; web + linux implement it, "
                       "the other five apps do not",
                tracked="the fake-cloud provisioning follow-up, Track 2",
            )
        # Override ONLY "dns" — pointed at the provider-shaped stub (Hetzner
        # DNS has a different wire envelope than Cloudflare). Do NOT override
        # "nest": the anonymous WsNestApi makes a "nest" override win for every
        # anonymous call, so the handle-check probe in go_to_dns_config would
        # try to WS-connect the (non-WS) fake nest and fail transiently. This
        # helper never provisions, so it doesn't need the nest redirect. The
        # reload re-mounts the wizard WITH the override before any verify probe.
        self.driver.set_provider_base_urls(
            {"dns": fake_cloud.dns_base_for(provider_id)}
        )

        self.go_to_dns_config()
        drv = self.driver
        # Same provider for VPS so continue_from_dns mirrors it.
        drv.call_machine_method("toggle_same_provider_for_vps", json.dumps(True))
        drv.call_machine_method("select_dns_provider", json.dumps(provider_id))
        # Hetzner exposes ONE Cloud api-token (kinds [vps, dns]) covering both
        # VPS and DNS. set_dns_cred takes (field_id, value) → JSON array.
        drv.call_machine_method("set_dns_cred", json.dumps(["api-token", "MOCK"]))
        # verify_dns honors provider_base_urls["dns"] → hits the fake's /zones.
        drv.call_machine_method("verify_dns")
        drv.call_machine_method("continue_from_dns")
        drv.wait_for("vps-config-back-button", timeout=15)

    def go_to_vps_config_with_verified(self,
                                       provider_id: str,
                                       creds: dict,
                                       fake_cloud) -> None:
        """Drive to vps_config and verify the VPS provider against
        ``fake_cloud`` so server types render as radios.

        Reaches vps_config via the DNS-deferred path
        (``dns-set-up-later-button``), selects the VPS provider, fills
        ``creds``, clicks Verify — ``verify_vps`` hits the fake's
        locations + server_types endpoints (redirected via
        ``provider_base_urls["vps"]``) and populates the server-type radios.

        Web + linux (see ``go_to_vps_config_with_dns_provider``). Here the
        VPS verify is driven by a real ``vps-verify-button`` UI click — linux's
        button already drives the async ``verify_vps`` on a worker runtime via
        ``run_on_tokio`` — so this helper needs only ``set_provider_base_urls``.
        """
        if not (self.driver.is_web() or self.driver.is_linux()):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="set_provider_base_urls",
                detail="fake-cloud provider redirection for the VPS verify; web + "
                       "linux implement it, the other five apps do not",
                tracked="the fake-cloud provisioning follow-up, Track 2",
            )
        # Override ONLY "vps". Overriding "nest" would make the anonymous
        # WsNestApi route the handle-check probe (in go_to_vps_config →
        # go_to_dns_config) at the non-WS fake nest → transient failure; this
        # helper never provisions, so it doesn't need the nest redirect.
        self.driver.set_provider_base_urls({"vps": fake_cloud.url_map()["vps"]})
        self.go_to_vps_config()
        self.driver.click(f"vps-provider-row[{provider_id}]")
        self.driver.wait_for("vps-credentials-form", timeout=10)
        for field_id, value in creds.items():
            self.driver.type_text(f"vps-credentials-form-{field_id}", value)
        # Verify arms only once the typed credentials have reached the form's
        # state; clicking it the instant the last keystroke lands drove a
        # control no user could press yet (a 409 "element is disabled" in the
        # 2026-09-22 linux sweep). Rendered is not enabled.
        self.driver.wait_until_enabled("vps-verify-button", timeout=10)
        self.driver.click("vps-verify-button")
        # verify_vps is async; the server-type radios render once it lands.
        self.driver.wait_for("vps-server-type-radio[0]", timeout=20)

    def go_to_vps_config_with_locations(self,
                                        provider_id: str,
                                        locations: list[dict]) -> None:
        """Drive to vps_config, select ``provider_id``, then inject verified
        VPS ``locations`` via the cross-app machine bridge so the
        ``vps-location-picker`` renders and is actuable.

        Unlike ``go_to_vps_config_with_verified`` (which runs the real
        ``verify_vps`` HTTP probe and is web-only until the other drivers
        gain ``set_provider_base_urls``), this uses
        ``set_vps_locations_for_test`` snapshot injection — the same pattern
        every other onboarding stage test uses — so it runs on every app.
        Web doesn't render the location picker, so callers gate to clients
        that do (currently Linux).

        ``locations`` is a list of ``{"id","name","city","country"}`` dicts.
        """
        self.go_to_vps_config()
        self.driver.click(f"vps-provider-row[{provider_id}]")
        self.driver.wait_for("vps-credentials-form", timeout=10)
        self.driver.call_machine_method(
            "set_vps_locations_for_test", json.dumps(locations)
        )
        self.driver.wait_for("vps-location-picker", timeout=10)

    def go_to_dns_post_instructions_with_records(self,
                                                  records: list[str]) -> None:
        """Land on dns_post_instructions with synthetic DNS records.

        Drives the wizard to ``OnboardingStep::DnsPostInstructions`` and
        seeds both the captured DNS records (``set_dns_records_for_test``)
        and a successful ``ProvisioningSnapshot`` (``set_provisioning_snapshot_for_test``)
        so ``m.dns_post_instructions()`` renders the markdown table the page
        surfaces — the same path the deferred-DNS orchestrator produces,
        minus the real cloud calls. Uses the cross-app
        ``call_machine_method`` E2E bridge (tracked internally), so subclasses
        don't need overrides.

        ``records`` are space-separated ``"TYPE NAME VALUE"`` strings
        (e.g. ``"A example.com 1.2.3.4"``); VALUE may contain spaces.
        """
        parsed: list[dict] = []
        for line in records:
            parts = line.split(None, 2)
            parsed.append({
                "record_type": parts[0] if parts else "A",
                "name": parts[1] if len(parts) > 1 else "example.com",
                "value": parts[2] if len(parts) > 2 else "",
                "ttl": 3600,
                "priority": None,
            })
        domain = parsed[0]["name"] if parsed else "example.com"
        provisioning_snapshot = {
            "overall": "Succeeded",
            "steps": [],
            "started_at_ms": None,
            "finished_at_ms": None,
            "result": {
                "server_id": "test-server-1",
                "ipv4": "203.0.113.5",
                "domain": domain,
                "claim_code": "test-claim-code",
            },
            "final_error": None,
        }
        # Seed the data before flipping the step so the page's first render
        # already has the records + provisioning result.
        self.driver.call_machine_method(
            "set_dns_records_for_test", json.dumps(parsed))
        self.driver.call_machine_method(
            "set_provisioning_snapshot_for_test", json.dumps(provisioning_snapshot))
        self.driver.call_machine_method(
            "set_step_for_test", json.dumps("DnsPostInstructions"))
        # Wait on the continue button as the page-entry canary: it's a
        # plain Button (definite AT-SPI role), unlike the dns-post-
        # instructions-text TextView which some toolkits surface less
        # reliably right after a rebuild.
        self.driver.wait_for("dns-post-instructions-continue-button", timeout=15)

    def go_to_awaiting_manual_dns(self,
                                  records: list[str] | None = None,
                                  handle: str = "alice@example.com",
                                  nest_url: str = "http://127.0.0.1:9",
                                  claim_code: str = "test-claim-code") -> None:
        """Land on the "Almost ready" (awaiting-manual-DNS) surface with seeded records.

        Seeds the machine straight to the deferred-DNS exit via
        ``seed_awaiting_manual_dns`` — the same call the relaunch-hydration path
        makes — so ``wizard_outcome() == AwaitingManualDns`` and the client
        renders the polling surface (NOT a wizard step). No real cloud
        provisioning needed. Uses the cross-app ``call_machine_method`` E2E
        bridge, so subclasses don't need overrides.

        ``records`` are space-separated ``"TYPE NAME VALUE"`` strings
        (e.g. ``"A nest.example.com 203.0.113.7"``); defaults to one A record.
        ``nest_url`` points at an unreachable loopback port so a stray recheck
        stays "still waiting" rather than reaching anything.
        """
        if records is None:
            records = ["A nest.example.com 203.0.113.7"]
        parsed: list[dict] = []
        for line in records:
            parts = line.split(None, 2)
            parsed.append({
                "record_type": parts[0] if parts else "A",
                "name": parts[1] if len(parts) > 1 else "example.com",
                "value": parts[2] if len(parts) > 2 else "",
                "ttl": 3600,
                "priority": None,
            })
        self.driver.call_machine_method("seed_awaiting_manual_dns", json.dumps({
            "nest_url": nest_url,
            "handle": handle,
            "dns_records": parsed,
            "claim_code": claim_code,
        }))
        # Surface-entry canary: the recheck button is a plain Button (definite
        # AT-SPI/AX role), unlike the status/records text elements which some
        # toolkits surface less reliably right after a rebuild.
        self.driver.wait_for("awaiting-dns-recheck-button", timeout=15)

    # NOTE: there are no `go_to_nest_login_*` helpers. The `nest_login` page
    # was deleted in the handle-first onboarding redesign — login is folded
    # into the Continue actions on `handle_entry` and `invite_request`
    # (docs/goal/behavior/onboarding.md § "Pages that do not exist"). The
    # outcomes the old helpers stood in for are covered by outcome-based
    # tests on the real pages:
    #   - AlreadyOnNest (welcome-back) / AlreadyOnNest{handle_differs}
    #     (change-handle) / NestRunningUserUnregistered (claimed-unregistered
    #     → invite_request): tests/test_handle_entry_outcomes.py
    #   - UnregisteredUnclaimedNest (unclaimed → claim_code):
    #     tests/test_claim_code_unclaimed_nest.py +
    #     tests/test_silent_challenge_unclaimed_nest.py
