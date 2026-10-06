"""Drive the T16 custody facet — "who holds my data" / "what I hold for others".

Spec: docs/goal/ui/devices.md § Custody facet (the Devices-page families:
``custody-holder-*`` owner side, ``custody-held-*`` + ``custody-offer-*`` host
side, ``custody-mint-*`` offer initiation) and docs/goal/ui/nests.md § Trust
facet — custody rows (the ``nest-trust-custody-*`` custodian-NEST family; the
nest-custodian identity fact owns the split). Mechanism:
docs/goal/architecture/account-data-plane.md § Replica posture → The custody
grant + ceremony.

The facet renders from the shared ``fauna-client-capabilities`` view-model
folds; the ceremony rides an existing 1:1 conversation, so tests establish one
first (``ConversationsActions.real_resolve_send_new``). Receipt/pump work is
anchored on the ``account_pump_cycles`` counters + the ``account_pump_now``
poke (``helpers.waiting``), never on cadence waits (convention 14).

Navigate to the surface first via ``BackupsActions.navigate_devices()`` (the
families render below the roster on Settings → Devices).
"""

from __future__ import annotations

import pytest

from drivers.base import PlatformDriver


class CustodyActions:
    """The custody facet's row families and gestures (tui-led, 2026-08-16/17)."""

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def require_supported(self) -> None:
        """Skip unless this app can drive the T16 ceremony — mint + accept,
        pieces 1+3 (convention 7 — the platform check lives in the action
        layer, never a test body).

        tui (2026-08-16/17), linux, windows and macOS + iOS (all 2026-09-28;
        apple through the shared FaunaKit ``DevicesContent``) render pieces
        1 + 3 and the mint with live controls. web is a DECLARED absence:
        accept, the mint and set-budget/stop are gated on capabilities web
        lacks (devices.md § Where logic lives). android has the UniFFI exports
        (``custody_accept`` … ``devices_keyless_posture``) but not yet the
        render — unbuilt debt, not an absence. Piece 2 (owner-side render,
        ``custody-holder-card``) is built on all seven apps."""
        if (
            self.driver.is_tui()
            or self.driver.is_linux()
            or self.driver.is_windows()
            or self.driver.is_macos()
            or self.driver.is_ios()
        ):
            return
        from helpers.app_surface import declared_absence, skip_unbuilt

        if self.driver.is_web():
            declared_absence(
                self.driver,
                capability="custody ceremony (accept, mint, set-budget/stop)",
                doc="ui/devices.md § Where logic lives",
            )
        skip_unbuilt(
            self.driver,
            surface="the T16 custody ceremony (mint + accept, pieces 1+3)",
            detail="the UniFFI face exports the acts and the keyless-posture "
                   "read; the per-app render is the remaining leg",
            tracked="devices.md § Implementation status today; the batched per-app trickle-down",
        )

    # ── offer initiation (owner side) ────────────────────────────────────────

    def open_mint(self) -> None:
        """Reveal the mint flow (``custody-mint-button``). A missing 1:1
        conversation surfaces on ``error-message`` (convention 11) — callers
        establish the conversation first."""
        self.driver.wait_for("custody-mint-button", timeout=10)
        self.driver.click("custody-mint-button")
        try:
            self.driver.wait_for("custody-mint-host-select", timeout=10)
        except TimeoutError as e:
            # Convention 6: an empty candidate list answers on error-message
            # (`devices.custody_mint_no_contacts`) — say which failure this is.
            error = (
                self.driver.get_text("error-message")
                if self.driver.is_visible("error-message")
                else ""
            )
            raise TimeoutError(f"{e} (error-message: {error!r})") from e

    def mint_offer(self, host_label: str) -> None:
        """Run the whole mint flow: open, pick ``host_label`` in the host
        select (the 1:1 conversation's thread label), confirm past the
        REQUIRED floor copy. The confirm records the offer through the config
        CAS and a drive pass posts it to the channel."""
        self.open_mint()
        assert self.driver.get_text("custody-mint-floor-note"), (
            "the custody-floor copy must render before the confirm "
            "(nests.md § Trust facet — custody rows: REQUIRED copy)"
        )
        self.driver.select("custody-mint-host-select", host_label)
        self.driver.wait_for("custody-mint-confirm-button", timeout=5)
        self.driver.click("custody-mint-confirm-button")

    # ── consent surface (host side) ──────────────────────────────────────────

    def offer_count(self) -> int:
        return self.driver.count("custody-offer-card")

    def offer_floor_note(self, index: int = 0) -> str:
        return self.driver.get_text("custody-offer-floor-note", index=index)

    def accept_offer(self, index: int = 0) -> None:
        """The explicit consent gesture — binds THIS device and posts the
        accept (the drive runs on the post-accept edge)."""
        self.driver.click("custody-offer-accept-button", index=index)

    def offer_has_nest_choice(self, index: int = 0) -> bool:
        """Whether the card renders the host-side target select — present
        ONLY under an offer advertising the nest binding AND with a pinned
        nest identity in hand (absent otherwise, never disabled)."""
        return self.driver.count("custody-offer-target-select") > index

    def accept_offer_on_nest(self, index: int = 0) -> None:
        """The NEST-form consent (the nest-custodian identity fact): pick
        "My nest" in the target select, then accept — the accept binds the
        host's PINNED nest identity and the ceremony drive deposits the
        custody-hosting row on the host's own nest."""
        from i18n.strings import S

        self.driver.select(
            "custody-offer-target-select",
            S.devices.custody_offer_target_nest,
            index=index,
        )
        self.driver.click("custody-offer-accept-button", index=index)

    def decline_offer(self, index: int = 0) -> None:
        self.driver.click("custody-offer-decline-button", index=index)

    # ── held-for-others rows (host side) ─────────────────────────────────────

    def held_count(self) -> int:
        return self.driver.count("custody-held-card")

    def held_owner(self, index: int = 0) -> str:
        return self.driver.get_text("custody-held-owner", index=index)

    def held_bytes(self, index: int = 0) -> str:
        """The row's metered line — ``Holding ‹held› of ‹cap›`` off this
        device's own latest minted receipt (dashes before the first)."""
        return self.driver.get_text("custody-held-bytes", index=index)

    def held_budget(self, index: int = 0) -> str:
        """The per-row budget input's current text: the persisted
        ``retained_bytes_cap`` as a byte size, re-seeded on every facet fold."""
        return self.driver.get_text("custody-held-budget-input", index=index)

    def set_held_budget(self, value: str) -> None:
        """Type a new ``retained_bytes_cap`` into the (first) per-row budget
        input and commit — the write lands on the registry row (the pump
        meters against it on its next pass). The input is an ``input_commit``:
        typing only writes the draft; the click fires ``CustodySetBudget`` (the
        type-then-click idiom a commit-on-Enter entry takes on every driver)."""
        self.driver.clear_and_type("custody-held-budget-input", value)
        self.driver.click("custody-held-budget-input")

    def stop_holding(self, index: int = 0) -> None:
        self.driver.click("custody-held-stop-button", index=index)

    # ── custodian rows (owner side, Devices page — device-anchored) ─────────

    def holder_count(self) -> int:
        return self.driver.count("custody-holder-card")

    def holder_receipt_status(self, index: int = 0) -> str:
        return self.driver.get_text("custody-holder-receipt-status", index=index)

    def holder_held_bytes(self, index: int = 0) -> str:
        return self.driver.get_text("custody-holder-held-bytes", index=index)

    def revoke_holder(self, index: int = 0) -> None:
        """Stop trusting this custodian (honest-bound copy renders on the
        card's own line; revoke runs nest-first per the pair-machine order)."""
        self.driver.click("custody-holder-revoke-button", index=index)
