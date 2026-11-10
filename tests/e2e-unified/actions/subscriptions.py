"""UI actions for the profile Tiers-tab SELF author management (subscriptions
Slice A) — drives the client profile page over the ``subscription-*`` /
``profile-*`` test IDs.

The profile page is the SELF per-user detail surface reached via the top-level
``profile-tab``; its Tiers tab hosts §1 My tiers / §2 Pending requests /
§3 Subscribers (apps/fauna-linux/src/views/profile/{mod,tiers}.rs). The §1/§2/§3
sections live in the inner stack's "tiers" page — hidden behind the default
"posts" child — so ``open_tiers_tab`` must run before any section read (the
automation prunes non-showing widgets).

The page is observer-free (manual re-read): it reloads on a mutation
(create/approve/reject/delete) and — mirroring the Peers page — whenever the
profile view becomes visible. So ``refresh`` bounces nav away and back to pick up
state changed by another actor (e.g. a freshly-arrived subscribe request).

Reference impl: linux (Slice-A lead). The other 5 clients lift the
same IDs (priority #1), so this action layer is platform-agnostic.
"""

from __future__ import annotations

import re
import time
from typing import TYPE_CHECKING

from helpers.waiting import account_pump_role, await_account_runtime_assembled

if TYPE_CHECKING:
    from drivers.base import PlatformDriver

# A rendered actor id: 32 bytes of blake3/ed25519 as lowercase hex.
_ACTOR_ID_RE = re.compile(r"[0-9a-f]{64}")


def _row_actor_id(text: str) -> str:
    """The actor id carried by a §2/§3 roster row's rendered text.

    A row element's text is not the bare id on every app: linux renders the
    request row as ``"<id> post-unlock-<hash> subscribe paid approve reject"``
    and the subscriber row as ``"<id> remove"``, folding in the row's own action
    labels. `pending_request_ids` and `subscriber_ids` both used to hand that
    whole string back while documenting themselves as returning the id, and
    their callers assert with exact list membership
    (``buyer_id in subscriber_ids()``), so the assertions could not be satisfied
    on any app whose row carries more than the id — while printing the correct
    id plainly inside the value they compared against.

    Extracting rather than splitting on whitespace keeps this app-agnostic: the
    id is found wherever the app places it in the row. Text with no 64-hex run
    falls back to itself, so an app whose row really is just the id is
    unchanged, and a genuinely empty/absent row still compares unequal rather
    than silently matching.
    """
    stripped = text.strip().lower()
    match = _ACTOR_ID_RE.search(stripped)
    return match.group(0) if match else stripped


class SubscriptionsActions:
    def __init__(self, driver: "PlatformDriver"):
        self.driver = driver

    # --- navigation ---

    def navigate(self) -> None:
        """Open the profile page (SELF). The Tiers-tab sections live here."""
        self.driver.set_state({"nav": {"stack": [{"view": "profile"}]}})
        self.driver.wait_for("profile-view", timeout=10.0)

    def open_tiers_tab(self) -> None:
        """Show the Tiers tab so §1/§2/§3 become visible (the inner stack's
        default child is "posts")."""
        self.driver.click("profile-tiers-tab")
        self.driver.wait_for("subscription-tiers-section", timeout=10.0)

    # --- e2e convention 7: app-gate declarations (the platform check lives
    # here, not the test body) ---

    def require_sell_post_seller_fanout_leg_supported(self) -> None:
        """Skip unless this app has both the §1 tier-create and §2 approve
        surfaces needed to prove the sell-post seller fan-out/reconcile leg.

        linux + tui (the tui Tiers author track lifted §§1–5) and windows today; widen
        with the trickle-down lifts. Windows joined
        2026-09-22: the gate had kept it out on the strength of a citation
        nobody had re-run, and the leg went green there unchanged — the Tiers
        tab's create form, §2 approve and roster surfaces are all built. Web
        joined 2026-10-09 on the same terms: its Tiers tab carries the same
        three surfaces, and the leg's witnesses ran there once the gate let
        them. macos and ios (shared FaunaKit ProfileView's Tiers tab, the same
        three surfaces) joined 2026-10-24 on the same terms."""
        from helpers.app_surface import require_surface

        require_surface(self.driver, "sell-post-seller-surfaces")

    def refresh(self) -> None:
        """Force a fresh read of all three sections.

        The page re-reads on becoming visible, so bounce nav to feed and back,
        then re-open the Tiers tab. Used after a *different* actor mutates state
        (a subscribe request the author's own UI didn't trigger)."""
        self.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        time.sleep(0.3)  # let the visible-child change settle
        self.navigate()
        self.open_tiers_tab()

    # --- §1 My tiers ---

    def create_tier(
        self,
        name: str,
        rank: int = 1,
        *,
        price_hint: str | None = None,
        description: str | None = None,
        payment_url: str | None = None,
        asking_price: str | None = None,
        auto_approve: bool = False,
    ) -> None:
        """Open the create form, fill it, and save (`tiers.create`).
        ``asking_price`` is the machine-comparable sats figure
        (`subscription-tier-form-asking-price`, `monetization.md` § The asking
        price), independent of the display ``price_hint``.

        On an app that hosts an account runtime, first waits for it to
        assemble: the tier's period key is custody on the account plane
        (`config-dissolution.md` — the `fauna.state.subscriptions` kind), and
        assembly runs off the login path — on web only once the inbound poll
        has built the conversations manager, which on a loaded box lands past
        :meth:`wait_for_tier`'s window. The create itself waits for the
        runtime too (``PeriodKeyStore::custody_for_write``); this barrier
        keeps the journey's own deadline from racing that wait."""
        if account_pump_role(self.driver) is not None:
            await_account_runtime_assembled(self.driver)
        self.driver.click("subscription-tier-create-button")
        self.driver.wait_for("subscription-tier-form", timeout=10.0)
        self.driver.clear_and_type("subscription-tier-form-name", name)
        self.driver.clear_and_type("subscription-tier-form-rank", str(rank))
        if description is not None:
            self.driver.clear_and_type("subscription-tier-form-description", description)
        if price_hint is not None:
            self.driver.clear_and_type("subscription-tier-form-price-hint", price_hint)
        if payment_url is not None:
            self.driver.clear_and_type("subscription-tier-form-payment-url", payment_url)
        if asking_price is not None:
            self.driver.clear_and_type("subscription-tier-form-asking-price", asking_price)
        if auto_approve:
            self.driver.click("subscription-tier-form-auto-approve")
        self.driver.click("subscription-tier-form-save")

    def tier_names(self) -> list[str]:
        n = self.driver.count("subscription-tier-row")
        return [self.driver.get_text("subscription-tier-name", index=i) for i in range(n)]

    def cancel_tier_form(self) -> None:
        """Close an open create/edit form without saving (``subscription-tier-form-cancel``)."""
        if self.driver.is_visible("subscription-tier-form"):
            self.driver.click("subscription-tier-form-cancel")

    def wait_for_tier(self, name: str, timeout: float = 10.0) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if name in self.tier_names():
                return True
            time.sleep(0.3)
        return name in self.tier_names()

    def tier_index(self, name: str) -> int:
        """The §1 row index of the tier named ``name``; raises when no row
        carries it (convention 6: name the rows that ARE there)."""
        names = self.tier_names()
        if name not in names:
            raise LookupError(f"no §1 tier row named {name!r}; rows={names!r}")
        return names.index(name)

    def tier_rank(self, name: str) -> str:
        """The rendered rank cell (`subscription-tier-rank`) of tier ``name``."""
        return self.driver.get_text("subscription-tier-rank", index=self.tier_index(name))

    def tier_price(self, name: str) -> str:
        """The rendered price cell (`subscription-tier-price`) of tier ``name``."""
        return self.driver.get_text("subscription-tier-price", index=self.tier_index(name))

    def edit_tier(
        self,
        name: str,
        *,
        rank: int | None = None,
        description: str | None = None,
        price_hint: str | None = None,
        payment_url: str | None = None,
        flip_auto_approve: bool = False,
    ) -> None:
        """Open tier ``name``'s edit form (`subscription-tier-edit-button`,
        prefilled from the row), replace the fields given, and save
        (`fauna.subscriptions.tiers.update`).

        Only REPLACES values: a field left ``None`` keeps its prefilled value.
        There is deliberately no "clear this field" argument — `tiers.update`
        has no clear verb (`monetization.md` § Implementation status today), so
        an emptied input means keep-current on every app and a caller asserting
        a cleared field would be asserting a promise the product does not make.
        ``flip_auto_approve`` clicks the toggle once."""
        self.driver.click("subscription-tier-edit-button", index=self.tier_index(name))
        self.driver.wait_for("subscription-tier-form", timeout=10.0)
        if rank is not None:
            self.driver.clear_and_type("subscription-tier-form-rank", str(rank))
        if description is not None:
            self.driver.clear_and_type("subscription-tier-form-description", description)
        if price_hint is not None:
            self.driver.clear_and_type("subscription-tier-form-price-hint", price_hint)
        if payment_url is not None:
            self.driver.clear_and_type("subscription-tier-form-payment-url", payment_url)
        if flip_auto_approve:
            self.driver.click("subscription-tier-form-auto-approve")
        self.driver.click("subscription-tier-form-save")

    def wait_for_tier_rank(self, name: str, rank: int, timeout: float = 10.0) -> bool:
        """Poll until tier ``name``'s rank cell reads ``rank`` — the §1 re-read
        after an edit lands asynchronously."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                if self.tier_rank(name).strip() == str(rank):
                    return True
            except LookupError:
                pass
            time.sleep(0.3)
        return False

    def delete_tier(self, name: str) -> None:
        """Delete tier ``name`` from its §1 row (`subscription-tier-delete-button`,
        `fauna.subscriptions.tiers.delete`)."""
        self.driver.click("subscription-tier-delete-button", index=self.tier_index(name))

    def wait_for_tier_gone(self, name: str, timeout: float = 10.0) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if name not in self.tier_names():
                return True
            time.sleep(0.3)
        return name not in self.tier_names()

    # --- §2 Pending requests ---

    def pending_request_count(self) -> int:
        return self.driver.count("subscription-request-row")

    def wait_for_pending_request(self, min_count: int = 1, timeout: float = 15.0) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.pending_request_count() >= min_count:
                return True
            time.sleep(0.5)
        return self.pending_request_count() >= min_count

    def approve_first_request(self) -> None:
        """Approve the first pending request → the transparent mint+upload
        (`SubscriptionsAuthor::approve_subscriber`), showing
        `subscription-request-busy` for its duration.

        Returns on the click, BEFORE the mint lands: wait on an observable
        (the roster, or `wait_for_pending_request_count`) before switching
        account, or the switch can drop the in-flight approve."""
        self.driver.click("subscription-request-approve-button")

    def approve_until(self, done, timeout: float = 90.0) -> bool:
        """Drive §2 until ``done()``, approving whatever is pending when observed.

        The app's own drain races every click: it grants `auto_approve` and
        `payment_entitled` rows with no judgment, on connect and then on a poll
        backstop whose cadence differs per app (30 s native, 2 s on web under the
        e2e agent — `apps/fauna-web/src/lib/subscriptionsAuthor.ts`). A row can
        therefore leave §2 before it is seen, or between being counted and being
        clicked. So ``done`` must read the OUTCOME (the grant landing, read from
        the nest), never the row, and a click that lost its row to the drain is
        not a failure: the next pass reads ``done`` again (convention 14)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if done():
                return True
            self.refresh()
            if self.pending_request_count() >= 1:
                try:
                    self.approve_first_request()
                except RuntimeError:
                    # A row still standing means the click really failed; a row
                    # gone means the drain took it.
                    if self.pending_request_count() >= 1:
                        raise
            time.sleep(1)  # sleep-ok: poll interval of a deadline loop
        return bool(done())

    def reject_request(self, index: int = 0) -> None:
        """Turn pending request ``index`` down (`subscription-request-reject-button`,
        `fauna.subscriptions.requests.reject`)."""
        self.driver.click("subscription-request-reject-button", index=index)

    def wait_for_pending_request_count(self, count: int, timeout: float = 15.0) -> bool:
        """Poll until §2 holds exactly ``count`` rows (the twin of
        `wait_for_pending_request`, which waits for AT LEAST n — a reject or
        approve needs the exact drop)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.pending_request_count() == count:
                return True
            time.sleep(0.5)
        return self.pending_request_count() == count

    def pending_request_ids(self) -> list[str]:
        """The hex actor id of every rendered §2 pending-request row — the
        buy-side twin of `subscriber_ids`, so a caller can tell a request from
        the WRONG actor (a purchase made by the outgoing session) apart from a
        correct request the approve then mis-rosters.

        ⚠ Unlike `subscription-subscriber-row`, whose text IS the bare hex id on
        every app, `subscription-request-row` renders the id *alongside* the
        tier name and its action labels — on linux the text reads
        ``"<id> post-unlock-<hash> subscribe paid approve reject"``. Returning
        that whole string broke this method's own contract and every caller
        written against it: `test_sell_post`'s identity assertion is
        ``buyer_id in pending_request_ids()``, an exact list-membership test,
        so it could never match and the linux arm failed with the CORRECT
        buyer id plainly visible inside the value it printed. So extract the id rather than hand back
        the row: a 64-hex-char run, wherever the app places it in the row.
        Rows with no such run fall back to the stripped text, which keeps any
        app whose row really is just the id working unchanged.
        """
        return [
            self._row_id("subscription-request-row", "subscription-request-subscriber", i)
            for i in range(self.pending_request_count())
        ]

    def _row_id(self, row_id: str, child_id: str, index: int) -> str:
        """The actor id of roster row ``index``: from the row's own text where
        the app folds it in (linux), else from the row's subscriber child
        (``child_id``, scoped to the row). tui paints a row container with no
        text of its own and carries the id on the child alone — measured
        2026-10-09, when both reads below returned ``['']`` there."""
        own = _row_actor_id(self.driver.get_text(row_id, index=index))
        if _ACTOR_ID_RE.fullmatch(own):
            return own
        try:
            return _row_actor_id(
                self.driver.get_text(child_id, scope=f"{row_id}[{index}]"))
        except LookupError:
            return own

    def request_paid(self, index: int = 0) -> bool:
        """Whether request ``index`` shows the "paid" badge
        (PendingRequest.payment_entitled — monetization.md § Pillar 3)."""
        return self.driver.is_visible(
            "subscription-request-paid-badge", scope=f"subscription-request-row[{index}]"
        )

    def hold_author_drain(self) -> None:
        """Hold this app's author drain: every subscribe request enqueued after
        this returns stays in §2 until :meth:`release_author_drain` — the one
        way to read a payment-backed row, which the pump otherwise grants on its
        next tick (convention 14). A test-agent command, never user-facing;
        gate the caller on the `author-drain-hold` surface."""
        reply = self.driver.call_command("subscriptions_hold_author_drain", {})
        assert not (isinstance(reply, dict) and reply.get("error")), reply

    def release_author_drain(self) -> None:
        """Release :meth:`hold_author_drain`; the next pump tick drains as usual.
        A no-op when nothing is held."""
        self.driver.call_command("subscriptions_release_author_drain", {})

    # --- §3 Subscribers roster ---

    def subscriber_count(self) -> int:
        return self.driver.count("subscription-subscriber-row")

    def wait_for_subscriber(self, min_count: int = 1, timeout: float = 20.0) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.subscriber_count() >= min_count:
                return True
            time.sleep(0.5)
        return self.subscriber_count() >= min_count

    def subscriber_ids(self) -> list[str]:
        """The hex actor id of every rendered §2 subscriber row.

        A COUNT of subscribers is not evidence that the right actor joined —
        exactly the weakness `helpers.e2e_session.login_as` warns about for the
        actor barrier ("a buyer/subscriber leg stayed green because the previous
        actor satisfies every assertion after it"). `subscription-subscriber-row`
        carries the subscriber's full hex id, so a caller can assert identity
        instead. Used by `test_sell_post`'s self-serve completion leg, which was
        green on a roster holding the WRONG actor (2026-08-25).

        ⚠ The row carries the id but is not ONLY the id — this line used to read
        "as its value on every app", and on linux the text measures as
        ``"<id> remove"``, carrying the row's own action label. Callers compare
        with ``buyer_id in subscriber_ids()``, exact list membership, so the
        extra token made the assertion unsatisfiable. Both extract the
        id through `_row_actor_id` now.
        """
        return [
            self._row_id("subscription-subscriber-row", "subscription-subscriber-handle", i)
            for i in range(self.subscriber_count())
        ]

    def remove_subscriber(self, index: int = 0) -> None:
        """Remove §3 roster row ``index`` (`subscription-subscriber-remove-button`)
        — the roster-rotating removal (`SubscriptionsAuthor::remove_subscriber`):
        it mints a reduced-roster KeyBlob under a fresh period key, so what the
        author posts to the tier afterwards no longer opens for the removed
        subscriber."""
        self.driver.click("subscription-subscriber-remove-button", index=index)

    def wait_for_subscriber_count(self, count: int, timeout: float = 20.0) -> bool:
        """Poll until the §3 roster holds exactly ``count`` rows."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.subscriber_count() == count:
                return True
            time.sleep(0.5)
        return self.subscriber_count() == count

    # --- §4 Payment providers (Pillar 3 — monetization.md § Pillars 2+3) ---

    def open_provider_form(self, kind: str | None = None) -> None:
        """Open the §4 add form; optionally select ``kind`` (the webhook-URL
        preview recomputes as soon as the kind select changes, before the
        provider is even saved)."""
        self.driver.click("subscription-provider-add-button")
        self.driver.wait_for("subscription-provider-form", timeout=10.0)
        if kind is not None:
            self.driver.select("subscription-provider-form-kind", kind)

    def close_provider_form(self) -> None:
        self.driver.click("subscription-provider-form-cancel")

    def add_provider(
        self, kind: str, webhook_secret: str, tier: str, product: str | None = None,
    ) -> None:
        """Open the §4 provider form, fill it, and save
        (`fauna.payments.providers.set`). ``product`` (optional) is the one
        product the tier is sold as — `subscription-provider-form-product`,
        the provider's own reference (Stripe: the Payment Link id); left
        ``None`` the field is not touched, so a fresh provider maps every
        verified payment (monetization.md § Pillar 3 → *Configuration*).
        ``kind`` and ``tier`` are display
        strings in the two selects (the kind select enumerates the shared
        `fauna-payments` registry; the tier select the author's own tiers)."""
        self.open_provider_form(kind)
        self.driver.clear_and_type("subscription-provider-form-secret", webhook_secret)
        self.driver.select("subscription-provider-form-tier-map", tier)
        if product is not None:
            self.driver.clear_and_type("subscription-provider-form-product", product)
        self.driver.click("subscription-provider-form-save")

    def provider_form_product(self) -> str:
        """The §4 form's product field as it reads right now — pre-filled from
        the listed provider when its kind is picked (not a secret)."""
        return self.driver.get_text("subscription-provider-form-product")

    def provider_kinds(self) -> list[str]:
        n = self.driver.count("subscription-provider-row")
        return [self.driver.get_text("subscription-provider-kind", index=i) for i in range(n)]

    def provider_status(self, index: int = 0) -> str:
        return self.driver.get_text("subscription-provider-status", index=index)

    def wait_for_provider_status(self, expected: str, index: int = 0, timeout: float = 10.0) -> bool:
        """Poll the shared evidence-based `provider_status_label` badge
        (`fauna_core::format::provider_status_label` — "Configured"/"Verified"/
        "Error") until it matches `expected`. Used after a webhook delivery,
        which stamps `last_verified_at`/`last_rejected_at` nest-side; the row
        only reflects it once the client re-fetches (see `refresh`)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.provider_status(index) == expected:
                return True
            time.sleep(0.3)
        return self.provider_status(index) == expected

    def wait_for_provider(self, kind: str, timeout: float = 10.0) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if kind in self.provider_kinds():
                return True
            time.sleep(0.3)
        return kind in self.provider_kinds()

    def remove_first_provider(self) -> None:
        """Remove the first configured provider (`fauna.payments.providers.remove`)."""
        self.driver.click("subscription-provider-remove-button")

    def wait_for_provider_gone(self, kind: str, timeout: float = 10.0) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if kind not in self.provider_kinds():
                return True
            time.sleep(0.3)
        return kind not in self.provider_kinds()

    def provider_webhook_url(self) -> str:
        """The §4 form's live webhook-URL preview — the exact URL to
        register at the provider's dashboard for the currently selected
        kind (recomputes as the kind select changes)."""
        return self.driver.get_text("subscription-provider-form-webhook-url")

    def provider_return_url(self) -> str:
        """The §4 form's live return-URL display — the post-checkout redirect
        to paste into the provider's checkout settings for the currently
        selected kind (recomputes as the kind select changes). Built on the
        creator's web-site origin, so it needs a public web address."""
        return self.driver.get_text("subscription-provider-form-return-url")

    # --- §5 Manual claim codes (Pillar 3 — monetization.md § Pillar 3) ---

    def mint_claim(self, tier: str) -> None:
        """Select ``tier`` in the §5 tier picker and mint a claim code
        (`fauna.payments.claims.mint`; provider always "manual")."""
        self.driver.select("subscription-claim-tier-select", tier)
        self.driver.click("subscription-claim-mint-button")

    def claim_codes(self) -> list[str]:
        n = self.driver.count("subscription-claim-row")
        return [self.driver.get_text("subscription-claim-code", index=i) for i in range(n)]

    def claim_tiers(self) -> list[str]:
        n = self.driver.count("subscription-claim-row")
        return [self.driver.get_text("subscription-claim-tier", index=i) for i in range(n)]

    def claim_status(self, index: int = 0) -> str:
        return self.driver.get_text("subscription-claim-status", index=index)

    def wait_for_claim(self, code: str, timeout: float = 10.0) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if code in self.claim_codes():
                return True
            time.sleep(0.3)
        return code in self.claim_codes()

    def wait_for_claim_count(self, min_count: int = 1, timeout: float = 10.0) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if len(self.claim_codes()) >= min_count:
                return True
            time.sleep(0.3)
        return len(self.claim_codes()) >= min_count

    def claim_void_text(self, index: int = 0) -> str:
        """The row's `subscription-claim-void-button` text — "Void", or, once
        armed, the consequence its second press has."""
        return self.driver.get_text(
            "subscription-claim-void-button", scope=f"subscription-claim-row[{index}]")

    def has_claim_void_button(self, index: int = 0) -> bool:
        return self.driver.count(
            "subscription-claim-void-button", scope=f"subscription-claim-row[{index}]") > 0

    def arm_claim_void(self, index: int = 0) -> None:
        """First press on the row's Void: arms the confirm in place (the same
        button's text turns into the consequence) — nothing is sent yet."""
        self.driver.click(
            "subscription-claim-void-button", scope=f"subscription-claim-row[{index}]")

    def confirm_claim_void(self, index: int = 0) -> None:
        """Second press on the armed row's Void: `fauna.payments.claims.void`."""
        self.driver.click(
            "subscription-claim-void-button", scope=f"subscription-claim-row[{index}]")

    def wait_for_claim_status(self, expected: str, index: int = 0, timeout: float = 10.0) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.claim_status(index) == expected:
                return True
            time.sleep(0.3)
        return self.claim_status(index) == expected

    # --- Slice B: consumer side (subscription-settings page) ---

    def navigate_settings(self) -> None:
        """Open the consumer `subscription-settings` page (Settings ▸
        Subscriptions). Switching the settings inner-stack to this sub-page fires
        its on-visible `mine.list` re-read (see `views/settings_shell.rs`)."""
        self.driver.set_state(
            {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "subscription-settings"}]}}
        )
        self.driver.wait_for("subscription-mine-section", timeout=10.0)

    def refresh_settings(self) -> None:
        """Force a fresh `mine.list` read. The page re-reads on becoming the
        visible sub-page, and the inner stack fires its notify only when the
        child *changes* — so bounce through a different sub-page first."""
        self.driver.set_state(
            {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}}
        )
        time.sleep(0.3)
        self.navigate_settings()

    def mine_count(self) -> int:
        return self.driver.count("subscription-mine-row")

    def mine_tiers(self) -> list[str]:
        n = self.driver.count("subscription-mine-row")
        return [self.driver.get_text("subscription-mine-tier", index=i) for i in range(n)]

    def mine_status(self, index: int = 0) -> str:
        return self.driver.get_text("subscription-mine-status", index=index)

    def mine_author(self, index: int = 0) -> str:
        return self.driver.get_text("subscription-mine-author", index=index)

    def wait_for_mine_subscription(self, tier: str, timeout: float = 20.0) -> bool:
        """Poll until `tier` shows on the consumer page, bouncing the sub-page to
        re-fire the on-visible read between polls (the read is async + may race
        the WS coming up after a fresh login)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if tier in self.mine_tiers():
                return True
            time.sleep(0.5)
            self.refresh_settings()
        return tier in self.mine_tiers()

    def unsubscribe_first(self) -> None:
        """Unsubscribe the first listed subscription (`fauna.subscriptions.
        unsubscribe`). Plaintext mode removes inline (the row vanishes on the
        re-read); encrypted mode returns `Queued` (the row stays until the author
        commits)."""
        self.driver.click("subscription-mine-unsubscribe-button")

    def redeem_claim(self, code: str) -> None:
        """Paste a post-payment claim code and redeem it
        (`fauna.payments.claims.redeem` — monetization.md § Pillar 3 Q4's
        universal fallback binding). Success re-reads the mine list (the
        queued entitlement renders exactly like a queued subscribe); typed
        errors surface via `error-message`."""
        self.driver.clear_and_type("subscription-claim-redeem-input", code)
        self.driver.click("subscription-claim-redeem-button")

    def wait_for_error(self, timeout: float = 10.0) -> str:
        """Poll until `error_text()` is non-empty; returns the error text
        (empty string on timeout)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            text = self.error_text()
            if text:
                return text
            time.sleep(0.3)
        return self.error_text()

    def wait_for_mine_empty(self, timeout: float = 15.0) -> bool:
        """Poll until the consumer list is empty (after an inline removal),
        bouncing the sub-page to re-fire the on-visible read between polls."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.mine_count() == 0:
                return True
            time.sleep(0.5)
            self.refresh_settings()
        return self.mine_count() == 0

    # --- shared error surface (Rule 2) ---

    def error_text(self) -> str:
        """Read the current error, preferring the state protocol
        (``messages.error``) — the canonical read (``ActionLayer.error_text``).
        The element-only read is unreliable on windows, whose ``error-message``
        is a 1px mirror holding a ``" "`` sentinel when idle (it keeps the UIA
        peer realized); the state read returns ``""`` when there is no active
        error. Falls back to the element for clients that don't serialize
        messages into state."""
        try:
            value = self.driver.get_state("messages.error")
            if value is not None:
                return str(value)
            messages = self.driver.get_state("messages")
            if messages is not None and "error" in messages:
                return ""  # messages serialized, no active error
        except Exception:
            pass
        if not self.driver.is_visible("error-message"):
            return ""
        try:
            return self.driver.get_text("error-message")
        except Exception:
            return ""
