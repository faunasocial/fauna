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

    def require_sell_post_authoring_supported(self) -> None:
        """Skip unless this app can author a sell-this-post unlock tier.

        Landed on tui (the lead app) + linux + web + android (monetization.md
        § Per-post pay-to-unlock). macos/ios joined 2026-08-02: the composer's
        gate select gained "Sell this post…" as its third answer, sharing the
        ordinary gate's teaser field and finishing through the identical
        upload + `submitGatedPost` pair. windows joined 2026-08-03 — the last
        of the 7 apps, closing the trickle-down."""
        if not (
            self.driver.is_tui()
            or self.driver.is_linux()
            or self.driver.is_web()
            or self.driver.is_android()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_windows()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="sell-this-post unlock-tier authoring",
                detail="landed on all 7 apps",
                tracked="monetization.md § Per-post pay-to-unlock",
            )

    def require_sell_post_buyer_leg_supported(self) -> None:
        """Skip unless this app has a §5 claim surface to prove the sell-post
        buyer leg (mint → redeem → unseal).

        Proven on linux + android + web + tui. web was previously blocked on the
        e2e test-agent actor-switch bug (a second `set_state` login within one
        test silently reverted to the FIRST actor, so the feed read through a
        retired WS client and came back empty); that bug is FIXED — the patch
        now moves the account registry with the legacy slot, so the registry's
        `mirrorActiveToLegacy()` can no longer heal the switch away.

        tui joined 2026-08-02: it already had the §5 MINT surface
        (the tui Tiers author track), and the consumer-side REDEEM input arrived with the
        `subscription-settings` page — this leg is mint → **redeem** → unseal, so
        it needed both halves.

        macos/ios joined 2026-08-02: both already had the full §1/§2/§3/§5
        profile Tiers-tab surface (`SubscriptionsVM`) and the consumer
        `subscription-settings` page — the only missing piece was the
        sell-post authoring leg above, which this widening rides.

        windows joined 2026-08-03 — the last of the 7 apps, closing the
        trickle-down: same shape as
        linux/android, composed off windows' existing full §1/§2/§3/§5
        profile Tiers-tab surface."""
        if not (
            self.driver.is_linux()
            or self.driver.is_android()
            or self.driver.is_web()
            or self.driver.is_tui()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_windows()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the §5 claim surface (sell-post buyer leg)",
                detail="proven on all 7 apps",
                tracked="",
            )

    def require_teaser_buy_supported(self) -> None:
        """Skip unless this app renders the self-serve teaser purchase
        affordance (`gated-post-price` / `gated-post-payment-link` /
        `gated-post-buy-button`, gap (2c), `monetization.md` § Per-post
        pay-to-unlock → the buyer's price read is post-addressed).

        Landed on tui (lead) + linux + web (2026-07-30) — the
        `FeedManager::resolve_post_unlock_offer` / `buy_unlock_offer` shared
        seam plus the per-app render. windows joined 2026-08-03
. macos + ios joined
 — new shared FaunaKit
        `PostUnlockOfferTeaser`, wired into both targets' feed list card.
        android joined last — the same shared
        `FeedManager` seam, a new `PostUnlockOfferTeaser` composable shared
        between the feed list card and `post_detail`. All 7 apps now render
        this affordance; the android arm is compile-verified only (the
        real e2e run rides the standing host-emulator gap, like every
        other android journey in this suite).

        ⚠ web's render is built and e2e-verified as the SELLER; the BUYER leg
        (`test_sell_post_teaser_price_and_self_serve_buy[web]`) is red on a
        pre-existing, separate actor-switch fragility this track did not
        introduce and has not root-caused — see the "web
        teaser-price buyer leg: gated-post-price never resolves" finding
        before assuming this gate means the buyer leg is proven there too."""
        if not (
            self.driver.is_tui()
            or self.driver.is_linux()
            or self.driver.is_web()
            or self.driver.is_windows()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_android()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the self-serve teaser buy affordance (gap (2c))",
                detail="landed on all 7 apps",
                tracked="monetization.md § Per-post pay-to-unlock",
            )

    def require_sell_post_seller_fanout_leg_supported(self) -> None:
        """Skip unless this app has both the §1 tier-create and §2 approve
        surfaces needed to prove the sell-post seller fan-out/reconcile leg.

        linux + tui (the tui Tiers author track lifted §§1–5) and windows today; widen
        with the trickle-down lifts. Windows joined
        2026-09-22: the gate had kept it out on the strength of a citation
        nobody had re-run, and the leg went green there unchanged — the Tiers
        tab's create form, §2 approve and roster surfaces are all built."""
        if not (self.driver.is_linux() or self.driver.is_tui() or self.driver.is_windows()):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the §1 tier-create + §2 approve surfaces (sell-post "
                        "seller fan-out leg)",
                detail="linux + tui + windows today",
                tracked="",
            )

    def require_consumer_subscriptions_page(self) -> None:
        """Skip unless this app builds the CONSUMER `subscription-settings`
        page — `subscription-mine-*` (My Subscriptions) and
        `subscription-claim-redeem-*`.

        Built on **all 7 apps** — linux (lead), web, android, apple, windows, and
        tui (2026-08-02, `apps/fauna-tui/src/settings/subscriptions.rs`), which
        closed the last declared gap `monetization.md` § Pillar 1 carried.

        The predicate is kept rather than deleted, and deliberately stays
        distinct from the Tiers-tab gates above: it names a different PAGE, not a
        different section of the profile, and folding the two together is exactly
        the conflation that kept three provider/mint tests skipped on tui after
        it had built the surfaces they exercise. An eighth surface (or an app
        that regresses the page) has one honest place to declare itself."""
        if not (
            self.driver.is_linux()
            or self.driver.is_web()
            or self.driver.is_android()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_windows()
            or self.driver.is_tui()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the consumer subscription-settings page "
                        "(subscription-mine-* / subscription-claim-redeem-*)",
                detail="built on all 7 apps as of 2026-08-02",
                tracked="monetization.md",
            )

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
        auto_approve: bool = False,
    ) -> None:
        """Open the create form, fill it, and save (`tiers.create`).

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
        if auto_approve:
            self.driver.click("subscription-tier-form-auto-approve")
        self.driver.click("subscription-tier-form-save")

    def tier_names(self) -> list[str]:
        n = self.driver.count("subscription-tier-row")
        return [self.driver.get_text("subscription-tier-name", index=i) for i in range(n)]

    def wait_for_tier(self, name: str, timeout: float = 10.0) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if name in self.tier_names():
                return True
            time.sleep(0.3)
        return name in self.tier_names()

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
        `subscription-request-busy` for its duration."""
        self.driver.click("subscription-request-approve-button")

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
            _row_actor_id(self.driver.get_text("subscription-request-row", index=i))
            for i in range(self.pending_request_count())
        ]

    def request_paid(self, index: int = 0) -> bool:
        """Whether request ``index`` shows the "paid" badge
        (PendingRequest.payment_entitled — monetization.md § Pillar 3)."""
        return self.driver.is_visible(
            "subscription-request-paid-badge", scope=f"subscription-request-row[{index}]"
        )

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
            _row_actor_id(self.driver.get_text("subscription-subscriber-row", index=i))
            for i in range(self.subscriber_count())
        ]

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

    def add_provider(self, kind: str, webhook_secret: str, tier: str) -> None:
        """Open the §4 provider form, fill it, and save
        (`fauna.payments.providers.set`). ``kind`` and ``tier`` are display
        strings in the two selects (the kind select enumerates the shared
        `fauna-payments` registry; the tier select the author's own tiers)."""
        self.open_provider_form(kind)
        self.driver.clear_and_type("subscription-provider-form-secret", webhook_secret)
        self.driver.select("subscription-provider-form-tier-map", tier)
        self.driver.click("subscription-provider-form-save")

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
