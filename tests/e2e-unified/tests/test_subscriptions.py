"""tier_3 e2e for the profile Tiers-tab SELF author management (subscriptions
Slice A) — `monetization.md` § Pillar 1; the client-minted broadcast-KeyBlob
upload-and-accept design (tracked internally).

The author creates a tier, a second (headless) actor subscribes — which ALWAYS
enqueues, there is no server-side mint
(`bins/fauna-nest/src/subscription_handlers.rs` `subscribe_handler` /
`tiers_create_handler`: "never mints a period key … the nest holds no mint
authority") — the author approves through the UI, and the approve transparently
mints + uploads a broadcast KeyBlob covering the new roster
(`SubscriptionsAuthor::approve_subscriber`). The subscriber then lands in the
roster with a readable KeyBlob.

No-modes retirement (ratified 2026-07-12): this used to be described as
"encrypted-mode only" — gated on the nest's storage mode (`StorageModeTag::
Encrypted`) — with a `plaintext` counterpart nest whose fresh tiers were
inline-server-minted instead. That axis is gone: every nest is sealed at rest
unconditionally now, and EVERY tier is client-minted: the nest holds no period
key at all (the nest-held legacy plane was removed by the compat-remnant
sweep). So a subscribe ALWAYS enqueues; there is no nest-side inline-grant
arm to reach. The
period-key custody lives in the `fauna.state.subscriptions` plane
(sealed by the client under the account's tip key, no MSEK), so
this suite needs a dedicated fresh nest but **no** mail bridge.

The queued-vs-approved design call (ratified 2026-08-01 — `monetization.md`
§ Pillar 1 → *The headless-author corollary + the canonical e2e seeding
shape*): a headless author can NEVER produce an `approved` subscription on a
client-minted tier — `auto_approve` removes creator judgment, never creator
presence — and no instant-grant test-hook exists, deliberately (it would seed
a state a post-Phase-4 nest cannot produce). The canonical seeding shape for
"an author-granted subscription exists" is: subscribe (assert `queued`) →
author's client online (its connect-time reconcile drains the queue) →
deadline-poll `status.get` until active → proceed as the subscriber.
`test_subscription_settings_lists_active_subscription_and_unsubscribes` below
consumes that shape as precondition seeding.

This is the first end-to-end proof of the whole client-minted mint+upload chain
through real binaries — the class of bug it catches is "the client mints a
payload the nest's 4-step verify rejects" (the subscription analogue of the
IMAP/CalDAV body-decrypt drift only tier_3 surfaces). The strongest assertion is
the subscriber-side `key_blob.get`: subscriber-gated AND requires a minted blob,
so its success means the full chain ran (period-key custody → self-signed
ManageSubscribers DeviceAuthorization → mint_key_blob → EncryptedKeyBlobUpload →
nest verify + store).

Linux is the Slice-A lead; the other 5 clients lift the same ui.yaml IDs after
(priority #1), at which point this test runs for them too.
"""

import time
import uuid

import pytest
from nacl.signing import SigningKey

from actions import ActionLayer
from actions.api_actor import ApiActor
from common.auth import create_actor_and_register
from helpers.app_surface import skip_unbuilt
from helpers.e2e_session import login_as
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

# The reserved rank-0 free tier "follow" subscribes to
# (`fauna_core::subscription::FOLLOWERS_TIER`, rank
# `FOLLOWERS_TIER_RANK == 0`, `auto_approve == true`). The nest auto-provisions
# it on the first follow (`subscription_handlers.rs::ensure_followers_tier`).
FOLLOWERS_TIER = "followers"

# Convention-14 named budget: how long seeding/proof waits for an author-side
# grant (a UI approve's mint+upload, or the author pump's drain — whose backstop
# cadence is 30s on apps without the `FAUNA_SUBS_POLL_SECS` override) to become
# visible in the subscriber's `status.get`. Generous by design; green runs pay
# only the actual latency.
AUTHOR_GRANT_BUDGET_S = 90


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _as_signing_key(raw) -> SigningKey:
    """Nests return the admin signing key as either a `SigningKey` or hex."""
    return raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))


@pytest.fixture
def subs_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest for the subscriptions suite (see module
    docstring) — every fresh tier is client-minted regardless of mode, so
    there is no longer a separate "encrypted" nest variant to spin up."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "subs-nest")
    yield nest
    cleanup()


def _login_as(app, nest, user, *, handle: str) -> None:
    """Drive the client into a logged-in session for `user` against `nest`,
    landing on the feed view — the same set_state login `logged_in_app` uses,
    re-pointed at this suite's own dedicated nest. Thin wrapper over the
    shared `helpers.e2e_session.login_as` — see that docstring for the
    barrier rationale (2026-08-10)."""
    login_as(app, nest, user, handle=handle, device_id="test-device-subs")


@pytest.mark.feature("subscriptions-and-tiers")
def test_subscription_approve_grants_keyblob(app, subs_nest):
    # Runs on every app that has landed the profile Tiers-tab AND can be
    # driven to it via the in-process set_state nav protocol (subscriptions
    # Slice A). linux is the lead; web + android + windows lifted it (same
    # ui.yaml IDs); macos lifted it; iOS's nav-route gap (the
    # `set_state({"nav":{"stack":[{"view":"profile"}]}})` no-op) is FIXED by
    # apple (the in-process reducer now routes the SELF
    # profile via MoreView→ProfileView) — all GREEN. tui lifted §§1–5 in the
    # tui Tiers author track, so all 7 apps now run this — the gate stays only
    # so a future 8th surface declares itself rather than failing opaquely.
    if not (
        app.driver.is_linux()
        or app.driver.is_web()
        or app.driver.is_android()
        or app.driver.is_windows()
        or app.driver.is_macos()
        or app.driver.is_ios()
        or app.driver.is_tui()
    ):
        skip_unbuilt(
            app.driver,
            surface="the subscriptions Slice-A author page (tiers-tab create-tier flow)",
            detail="all 7 apps have it. android --client e2e is "
            "host-emulator-gated fleet-wide (internal dev note)",
            tracked="monetization.md",
        )

    admin_sk = _as_signing_key(subs_nest["admin"]["signing_key"])
    # Author (drives the UI) + subscriber (headless), both regular registered
    # actors on the dedicated nest.
    author = create_actor_and_register(
        subs_nest["port"], base_url=subs_nest["url"], admin_signing_key=admin_sk,
    )
    subscriber = create_actor_and_register(
        subs_nest["port"], base_url=subs_nest["url"], admin_signing_key=admin_sk,
    )

    _login_as(app, subs_nest, author, handle="e2e-author")

    subs = app.subscriptions
    tier = _unique("gold")

    # ── Author: create the tier. ────────────────────────────────────────────
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=1, price_hint="$5/mo")
    assert subs.wait_for_tier(tier), (
        f"created tier {tier!r} should appear in §1 My tiers; error={subs.error_text()!r}"
    )

    # ── Second actor subscribes — a fresh tier ALWAYS enqueues (no server-side
    # mint; see module docstring). ────────────────────────────────────────────
    sub_actor = ApiActor(
        subs_nest["url"], subscriber["token"], subscriber["actor_id_hex"],
        bytes(subscriber["signing_key"]),
    )
    author_id = author["actor_id_bytes"]
    reply = sub_actor.subscribe(author_id, tier)
    assert reply.get("outcome") == "queued", (
        f"subscribing to a fresh tier should queue (no server-side mint), got {reply!r}"
    )

    # ── Author: the pending request appears (page re-reads on becoming visible). ──
    subs.refresh()
    assert subs.wait_for_pending_request(1), (
        f"the queued subscribe should surface in §2 Pending requests; "
        f"error={subs.error_text()!r}"
    )

    # ── Author: approve → transparent mint + upload of the broadcast KeyBlob. ──
    subs.approve_first_request()
    assert subs.wait_for_subscriber(1), (
        f"after approve the subscriber should join the §3 roster; error={subs.error_text()!r}"
    )

    # ── The mint ran end-to-end: the subscriber reads the minted KeyBlob, and ──
    # their status flips to the active tier.
    blob = sub_actor.subscription_key_blob(author_id, tier)
    assert blob["version"] >= 1, (
        f"the subscriber should read a minted KeyBlob (version >= 1) — proof the "
        f"author's encrypted mint+upload landed; got {blob!r}"
    )
    status = sub_actor.subscription_status(author_id)
    assert status.get("tier") == tier, (
        f"the subscriber's status should show the active tier {tier!r}, got {status!r}"
    )


@pytest.mark.feature("subscriptions-and-tiers")
def test_follow_auto_grants_in_encrypted_mode(app, subs_nest):
    """Frictionless FOLLOW: the author's client auto-grants a queued follow
    with NO manual approve — `monetization.md` § The unifying model (grant
    path 2, "auto_approve … no creator action") + § Pillar 1. (Test name kept
    as-is post no-modes-retirement — the behavior it proves, client-minted
    auto-grant, is now the universal path, not an "encrypted mode" special case.)

    The nest can't mint the KeyBlob itself, so a follow — subscribe to
    the reserved free rank-0 `followers` tier, which the nest auto-provisions with
    `auto_approve=true` — still ENQUEUES (`Queued`). The grant needs the author's
    client to mint: on connect its per-app run-wiring runs
    `SubscriptionsAuthor::resume_pending_removals` + `drain_auto_approvals` (a
    connect-time pass + a poll backstop), which mints + uploads the followers
    KeyBlob and grants the queued follow with zero author UI action.

    The follower's subscribe is headless PRECONDITION setup (an `ApiActor` raw
    WS-RPC subscribe under the follower's own key); the UI subscribe path
    (`profile-follow-button` → subscribe) is proven separately by
    `test_profile.py::test_other_profile_offers_render_and_subscribe`. The
    behavior under test here is the AUTHOR's headless auto-approve loop, so the
    only client action is the author coming online (the standard set_state
    login) — there is no author UI mutation to drive.

    Client gate: NONE — all 7 apps now run the on-connect drain
    (`monetization.md` § Pillar 1 table, "Author pump run-wiring" row). linux
    (lead, `apps/fauna-linux/src/subscriptions_author.rs`), windows
    (`SubscriptionsAuthorPump`), web (`apps/fauna-web/src/lib/subscriptionsAuthor.ts`,
    started from `+layout.svelte`), android (`ApiClient.startSubscriptionsAuthorPump`,
    started from the `ensureNestConnected` connect chokepoint), and macos/ios
    (`SubscriptionsAuthorCadence`, shared FaunaKit, closed 2026-08-21) are all
    wired to the single shared tick (`subscriptions_reconcile_once`). NOTE:
    android is CODE-wired + compile-verified but its e2e is host-emulator-gated
    (deselected on the other dev machines; internal dev note), so this
    assertion has not yet been observed green for android — it runs once the
    host emulator is stood up."""
    admin_sk = _as_signing_key(subs_nest["admin"]["signing_key"])
    # Author (its client runs the loop) + follower (headless), both regular
    # registered actors on the encrypted nest.
    author = create_actor_and_register(
        subs_nest["port"], base_url=subs_nest["url"], admin_signing_key=admin_sk,
    )
    follower = create_actor_and_register(
        subs_nest["port"], base_url=subs_nest["url"], admin_signing_key=admin_sk,
    )
    author_id = author["actor_id_bytes"]

    # ── Follower subscribes to the reserved free `followers` tier BEFORE the ──
    # author is online. Encrypted mode ALWAYS enqueues (no server-side mint); the
    # nest auto-provisions the rank-0 `followers` tier (auto_approve=true).
    follower_actor = ApiActor(
        subs_nest["url"], follower["token"], follower["actor_id_hex"],
        bytes(follower["signing_key"]),
    )
    reply = follower_actor.subscribe(author_id, FOLLOWERS_TIER)
    assert reply.get("outcome") == "queued", (
        f"a follow should enqueue (no server-side mint), got {reply!r}"
    )
    # Not granted yet — the author's client has not run the drain.
    pre = follower_actor.subscription_status(author_id)
    assert pre.get("tier") != FOLLOWERS_TIER, (
        f"the follow should be Queued (ungranted) before the author connects, got {pre!r}"
    )

    # ── Author's client comes online → its on-connect run-wiring spawns the ──
    # auto-approve loop → the connect-time drain (resume_pending_removals +
    # drain_auto_approvals, plus a poll backstop) mints the followers KeyBlob and
    # grants the queued follow, with NO manual approve.
    _login_as(app, subs_nest, author, handle="e2e-author")

    # ── The grant appears on its own (no `subscription-request-approve-button` ──
    # click). Poll the follower's status until the author loop drains it.
    deadline = time.time() + AUTHOR_GRANT_BUDGET_S
    granted_status = None
    while time.time() < deadline:
        status = follower_actor.subscription_status(author_id)
        if status.get("tier") == FOLLOWERS_TIER:
            granted_status = status
            break
        time.sleep(2)  # sleep-ok: poll interval of a deadline loop
    assert granted_status is not None, (
        "the queued follow was never auto-approved by the author's "
        "client loop (drain_auto_approvals) — no manual approve; the per-app "
        "run-wiring is missing or broken"
    )

    # ── The full mint chain ran: the follower reads the minted followers KeyBlob. ──
    blob = follower_actor.subscription_key_blob(author_id, FOLLOWERS_TIER)
    assert blob["version"] >= 1, (
        f"the follower should read a minted followers KeyBlob (version >= 1) — proof "
        f"the author loop's encrypted mint+upload landed; got {blob!r}"
    )


# ── The two-GUI pump proof (below) ──────────────────────────────────────────
#
# The author's pump cadence for the second GUI app. `monetization.md`
# § Pillar 1 → *Where the logic lives* names the production backstop ("the 30 s
# constant plus the `FAUNA_SUBS_POLL_SECS` e2e override; native-only env probe"),
# and this is the first test to actually set that override — it exists for the
# e2e and no suite was passing it, so the poll arm has been running at 30 s
# wherever a test touched it at all.
#
# NOTE this is a *cadence*, not a timeout: nothing below asserts on it. It only
# decides how long the (bounded, deadline-polled) green path waits.
AUTHOR_PUMP_POLL_SECS = 2


@pytest.fixture
def subs_author_app(request, app, subs_nest):
    """A SECOND GUI app — the **author** — on this suite's nest, logged in and
    running its subscriptions pump at a fast cadence.

    Yields ``(author_app, author_actor)``. The author drives its own UI only to
    create the tier; the behavior under test needs it merely to be *online*, so
    the fixture's whole job is "a real client of this app kind exists for the
    author and its pump is ticking".

    Built **directly** (NOT via the session ``_driver_cache``, which is
    single-app), mirroring ``second_real_faunamls_app``; convention 10 gives each
    launch its own isolated world, so the two apps share nothing but the nest.

    Native-only. The env probe behind ``FAUNA_SUBS_POLL_SECS`` is native by
    construction (a browser has no environment — ``author_poll_secs()``), so web
    would run this at the 30 s constant; and only the two direct-Rust apps
    (tui — the lead — and linux) are cheap to launch twice on one machine.
    """
    from conftest import _build_app_config
    from drivers import create_driver

    subscriber_driver = app.driver
    if subscriber_driver.is_tui():
        app_name = "tui"
    elif subscriber_driver.is_linux():
        app_name = "linux"
    else:
        skip_unbuilt(
            subscriber_driver,
            surface="a second, author-side GUI client with a settable pump cadence",
            detail="the FAUNA_SUBS_POLL_SECS probe is native-only (a browser has "
            "no environment), and the two-GUI launch is proven on the direct-Rust "
            "apps tui (lead) + linux",
            tracked="monetization.md",
        )

    admin_sk = _as_signing_key(subs_nest["admin"]["signing_key"])
    author = create_actor_and_register(
        subs_nest["port"], base_url=subs_nest["url"], admin_signing_key=admin_sk,
    )

    config = _build_app_config(app_name, subs_nest, request)
    config.setdefault("environment", {})
    config["environment"]["FAUNA_SUBS_POLL_SECS"] = str(AUTHOR_PUMP_POLL_SECS)

    driver = create_driver(app_name)
    driver.launch(config)
    try:
        author_app = ActionLayer(driver)
        # The same set_state login every other test in this file uses, against
        # this suite's own nest. `session::establish` is what spawns the pump, so
        # a completed login is also what puts the author's client "online" in the
        # sense this test means.
        _login_as(author_app, subs_nest, author, handle="e2e-pump-author")
        yield author_app, author
    finally:
        try:
            driver.screenshot("teardown-subs-author")
        except Exception:
            pass
        driver.teardown()


@pytest.mark.feature("subscriptions-and-tiers")
def test_the_author_pumps_poll_backstop_grants_a_ui_subscribe_with_no_author_action(
    app, subs_author_app, subs_nest
):
    """A subscriber subscribes **through their own UI** to an author who is
    **already online**, and the row reaches "Subscribed" with the author never
    touching their app — `monetization.md` § Pillar 1 → *Where the logic lives*
    ("`author_poll_interval()` is the backstop cadence … both halves run on every
    tick") and the matrix row naming the pump's "connect + the shared 30 s
    backstop".

    Two GUI apps, and what each contributes:

    * the **author's** app exists only to run the pump. It creates the tier
      through its own UI — mandatory, not stylistic: a raw WS-RPC `tiers.create`
      mints an ORPHAN tier whose period key lands in no custody, so nothing could
      ever mint its KeyBlob (module docstring). After that the author's UI is
      never touched again, which is what makes the grant attributable to the pump
      and nothing else.
    * the **subscriber's** app performs the mutation under test — the offer-row
      Subscribe click — and is the surface the assertion reads.

    **What this covers that nothing else did.** Two existing tests bracket this
    one and neither reaches it. `test_profile.py::test_other_profile_offers_
    render_and_subscribe` drives the same UI subscribe but its author is a
    headless `ApiActor` with no client, so it can only ever assert the flow's
    Queued end. `test_follow_auto_grants_in_encrypted_mode` above proves a drain,
    but headlessly (an `ApiActor` subscribe) and via the author's **connect-time**
    pass — it logs the author in *after* the queue is non-empty, so a pump that
    only ever drained on connect would pass it. Here the author is established
    before the request exists, so the connect pass has already run against an
    empty queue and only a later **poll** tick can explain the grant.

    That ordering is a causal barrier, not a wait (convention 14): the author's
    tier is asserted present in their own §1 list *before* the subscriber
    subscribes, and the tier cannot render without the login that spawns the pump
    (`session::establish` → `subscriptions_author::start`).

    Cross-app: the subscriber leg is the already-lifted OTHER-profile offers
    browse (6 of 7 apps); the author leg needs a second native launch, so the
    fixture gates to tui (lead) + linux.
    """
    subscriber_app = app
    subscriber_app.profile.require_other_profile_offers_browse_supported()
    author_app, author = subs_author_app
    author_id = author["actor_id_bytes"]

    admin_sk = _as_signing_key(subs_nest["admin"]["signing_key"])
    subscriber = create_actor_and_register(
        subs_nest["port"], base_url=subs_nest["url"], admin_signing_key=admin_sk,
    )

    # ── Author: create the auto_approve tier through their OWN UI. ────────────
    tier = _unique("gold")
    author_app.subscriptions.navigate()
    author_app.subscriptions.open_tiers_tab()
    author_app.subscriptions.create_tier(
        tier, rank=1, auto_approve=True, price_hint="$5/mo",
    )
    assert author_app.subscriptions.wait_for_tier(tier), (
        f"the author's tier {tier!r} should appear in their §1 My tiers — this is "
        f"also the barrier proving their session (and so their pump) is up before "
        f"the subscribe below; error={author_app.subscriptions.error_text()!r}"
    )

    # ── Subscriber: accept the author as a contact (the only wired tap-through ──
    # to another actor's profile), then log in and open that profile.
    subscriber_actor = ApiActor(
        subs_nest["url"], subscriber["token"], subscriber["actor_id_hex"],
        bytes(subscriber["signing_key"]),
    )
    subscriber_actor.accept_knock(author["actor_id_hex"])
    _login_as(subscriber_app, subs_nest, subscriber, handle="e2e-pump-subscriber")

    subscriber_app.contacts.open_contact_profile(author["actor_id_hex"])
    subscriber_app.driver.wait_for("profile-view", timeout=10.0)
    subscriber_app.profile.open_tiers_tab()
    assert subscriber_app.profile.wait_for_offer(tier), (
        f"the author's offered tier {tier!r} should render in the subscriber's "
        f"offers section; count={subscriber_app.profile.offer_count()}, "
        f"error={subscriber_app.profile.error_text()!r}"
    )

    # ── The mutation under test: subscribe from the offer row. Nothing after ──
    # this line touches the author's app.
    subscriber_app.profile.subscribe_offer(0)

    # ── The row reaches "Subscribed" on its own. There is no push kind for a ──
    # subscribe grant, so the subscriber has to re-activate the Tiers tab to
    # re-read — the ruled uniform door (`monetization.md` § Pillar 1 → *The
    # Tiers-tab re-read door*), which the helper owns and this call therefore
    # pins per app; the budget is the same named one the rest of this file uses.
    #
    # The pre-grant "Pending approval" transient is deliberately NOT asserted: it
    # is a race against the very pump under test (a fast tick can grant before
    # the click's own refetch settles), and asserting it would make this test
    # timing-dependent for no coverage — the Queued end of the flow is already
    # pinned wire-side by test_subscription_approve_grants_keyblob and UI-side by
    # test_other_profile_offers_render_and_subscribe.
    seen = subscriber_app.profile.wait_for_offer_status_after_external_grant(
        S.subscriptions.offer_status_active, timeout=AUTHOR_GRANT_BUDGET_S,
    )
    assert seen == S.subscriptions.offer_status_active, (
        f"the subscriber's offer row should reach {S.subscriptions.offer_status_active!r} "
        f"with NO author UI action — the author's client was already online when "
        f"the request arrived, so its pump's poll backstop is the only thing that "
        f"can grant it. The row reads {seen!r}; "
        f"error={subscriber_app.profile.error_text()!r}"
    )

    # ── The rendered flip is backed by the real mint chain, not a local guess: ──
    # the subscriber reads the minted KeyBlob (subscriber-gated AND requires a
    # minted blob — the module docstring's strongest assertion).
    blob = subscriber_actor.subscription_key_blob(author_id, tier)
    assert blob["version"] >= 1, (
        f"the subscriber should read a minted KeyBlob (version >= 1) — proof the "
        f"pump's mint+upload actually ran behind the rendered status; got {blob!r}"
    )


@pytest.mark.feature("subscriptions-and-tiers")
def test_ui_subscribe_publishes_pq_hybrid_encapsulation_key(app, subs_nest):
    """Clicking Subscribe on another actor's offer row through the real
    CLIENT publishes the subscriber's ML-KEM-768 encapsulation key — proving
    the whole chain from `ProfileActions.subscribe_offer`'s underlying call
    (`subs.subscribe_publishing_ek`, e.g. `apps/fauna-linux/src/views/
    profile/offers.rs`) through the wire to the nest's persisted
    `subscribe_requests.mlkem_encaps_key` column
    (`docs/goal/architecture/security/post-quantum.md` § Implementation
    status → Subscription `KeyBlobEntry`, surface B slice S4b).

    Clients publish the ek unconditionally (no capability token) — no
    special nest config is needed. The gate this test
    closes is purely "does the real client publish the ek when a human
    clicks Subscribe", which nothing else proved:
    `conformance_subscription_keyblob_pq.rs` (tier_3, nest-only) dispatches
    `fauna.subscriptions.subscribe` directly with a hand-built
    `mlkem_encaps_key`, never through a real client; the UI-subscribe tests
    elsewhere (`test_profile.py::test_other_profile_offers_render_and_
    subscribe`, `test_the_author_pumps_poll_backstop_grants_a_ui_subscribe_
    with_no_author_action` above) drive the same click but never inspect the
    published key.

    The author is a headless `ApiActor` — proving the CLIENT half (the
    subscriber) needs no author-side UI at all; the tier is seeded via the
    ORPHAN-tier raw call (`subscription_create_tier`'s own warning: fine for
    render-only seeding, never for a test needing a grant to materialize —
    this test asserts on the raw `subscribe_requests` row, not a grant, so
    the orphan tier is exactly right here and the second-native-GUI pump
    machinery above is unnecessary).
    """
    subscriber_app = app
    subscriber_app.profile.require_other_profile_offers_browse_supported()

    admin_sk = _as_signing_key(subs_nest["admin"]["signing_key"])
    author = create_actor_and_register(
        subs_nest["port"], base_url=subs_nest["url"], admin_signing_key=admin_sk,
    )
    author_actor = ApiActor(
        subs_nest["url"], author["token"], author["actor_id_hex"],
        bytes(author["signing_key"]),
    )
    tier = _unique("pq")
    author_actor.subscription_create_tier(tier, rank=1)

    subscriber = create_actor_and_register(
        subs_nest["port"], base_url=subs_nest["url"], admin_signing_key=admin_sk,
    )
    subscriber_actor = ApiActor(
        subs_nest["url"], subscriber["token"], subscriber["actor_id_hex"],
        bytes(subscriber["signing_key"]),
    )
    # The only wired tap-through to another actor's profile (mirrors the pump
    # test above).
    subscriber_actor.accept_knock(author["actor_id_hex"])
    _login_as(subscriber_app, subs_nest, subscriber, handle="e2e-pq-subscriber")

    subscriber_app.contacts.open_contact_profile(author["actor_id_hex"])
    subscriber_app.driver.wait_for("profile-view", timeout=10.0)
    subscriber_app.profile.open_tiers_tab()
    assert subscriber_app.profile.wait_for_offer(tier), (
        f"the author's offered tier {tier!r} should render in the subscriber's "
        f"offers section; count={subscriber_app.profile.offer_count()}, "
        f"error={subscriber_app.profile.error_text()!r}"
    )

    # ── The mutation under test: subscribe from the offer row. ──────────────
    subscriber_app.profile.subscribe_offer(0)

    # ── The nest ALWAYS enqueues a fresh client-minted tier's subscribe ──────
    # regardless of auto_approve (no-modes retirement, module docstring), and
    # no author client is running to drain the queue — so the row sits in
    # `requests.list` until this test reads it. Deadline-poll: the click's
    # WS round trip is async from the test's perspective.
    deadline = time.time() + 15.0
    entry = None
    while time.time() < deadline:
        for r in author_actor.subscription_requests_list():
            if bytes(r["subscriber_id"]) == subscriber["actor_id_bytes"]:
                entry = r
                break
        if entry is not None:
            break
        time.sleep(0.3)

    assert entry is not None, (
        f"the author should see a pending request from the subscriber after "
        f"the UI subscribe click; error={subscriber_app.profile.error_text()!r}"
    )
    ek = entry.get("mlkem_encaps_key")
    assert ek is not None, (
        f"the request the real CLIENT published should carry an ML-KEM ek "
        f"(clients publish the ek unconditionally (no capability token), and "
        f"subscribe_publishing_ek is the wired call behind the Subscribe "
        f"button) — got entry={entry!r}"
    )
    assert len(ek) == 1184, (
        f"the published ek should be a 1184-byte ML-KEM-768 encapsulation "
        f"key; got {len(ek)} bytes"
    )


@pytest.fixture
def subs_consumer_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest for the consumer-page test.

    Historical: this used to be a nest committed to PLAINTEXT storage mode,
    whose inline-grant arm let the test below seed an active subscription
    headlessly. That arm is retired (no-modes, 2026-07-12); the test now seeds
    via the canonical author-client-drain shape (module docstring), and this
    stays a plain fresh nest."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "subs-consumer-nest")
    yield nest
    cleanup()


@pytest.mark.feature("subscriptions-and-tiers")
def test_subscription_settings_lists_active_subscription_and_unsubscribes(app, subs_consumer_nest):
    """Slice B consumer page: a subscriber's `subscription-settings` page lists
    their active subscription via the caller-scoped `fauna.subscriptions.mine.list`
    enumeration, and the unsubscribe button drops it.

    Seeding follows the canonical author-client shape (ratified 2026-08-01 —
    `monetization.md` § Pillar 1 → *The headless-author corollary + the
    canonical e2e seeding shape*; module docstring): the AUTHOR'S APP creates
    the tier (a raw WS-RPC `tiers.create` would mint an orphan tier — the
    period key lands in no custody, so nothing could ever mint its KeyBlob;
    `approve_subscriber` auto-heals a missing key only for `followers`), the
    subscriber's subscribe QUEUES (a client-minted tier can never grant
    inline), the author approves through the UI (grant path 1 — deterministic,
    no drain-cadence dependence; every method here is per-app-proven by
    test_subscription_approve_grants_keyblob), and an API-side deadline poll
    confirms the grant before the subscriber logs in. The author leg is
    precondition setup; the behavior under test is the consumer page render +
    UI unsubscribe, driven as the subscriber.

    The unsubscribe leg mirrors the same rule: the leave QUEUES (the row
    honestly persists — an app that optimistically drops it is wrong), the
    author's client comes online once more and its reconcile commits the
    removal rotation with no judgment (the drain's unsubscribe-commit leg,
    2026-08-01), and only then is the consumer list empty.

    ⚠ The author→subscriber sequence makes this a second-login-as-a-different-
    actor test on every app — the exact shape the actor-switch fixes
    covered on linux/web/android/tui. windows + apple carry unverified twins
    (their verification legs ride the 2026-08-01 trickle-down batches); if this
    test wedges there with an empty error-message, suspect the test agent's
    set_state actor switch before the product.

    All 7 apps implement the consumer page (linux lead; android/web/windows/
    macos/ios lifted, tui last on 2026-08-02 — same ui.yaml IDs throughout). The
    conftest deselects clients not available on the current machine (android e2e
    is host-emulator-gated fleet-wide; internal dev note). Apple render-confirm
    is entrusted to the apple in-process driver render-gate."""
    # The page gate is kept though every app now passes it: it names a PAGE
    # distinct from the Tiers-tab author management (`monetization.md` § Pillar
    # 1), so a regression or an eighth surface reports as "this app lacks the
    # consumer page" rather than dying at the API precondition above and hiding
    # *which* of the two reasons it failed for.
    app.subscriptions.require_consumer_subscriptions_page()
    admin_sk = _as_signing_key(subs_consumer_nest["admin"]["signing_key"])
    # `with_handle=False` is load-bearing for the creator-column assertion
    # below: this test exercises the empty-handle→hex fallback, so the author
    # must be admitted the handle-LESS way (the shape `public-mode.md`
    # § *A handle-less account* describes).
    author = create_actor_and_register(
        subs_consumer_nest["port"], base_url=subs_consumer_nest["url"], admin_signing_key=admin_sk,
        with_handle=False,
    )
    subscriber = create_actor_and_register(
        subs_consumer_nest["port"], base_url=subs_consumer_nest["url"], admin_signing_key=admin_sk,
    )
    tier = _unique("gold")

    # ── Precondition seeding, the canonical author-client shape (module ──────
    # docstring). The author's app creates the tier so the period key lands in
    # their sealed `fauna.state.subscriptions` custody; the handle-less author renders via the
    # hex fallback throughout, a product-supported state (public-mode.md
    # § A handle-less account).
    _login_as(app, subs_consumer_nest, author, handle="")
    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=1, price_hint="$5/mo")
    assert subs.wait_for_tier(tier), (
        f"seeding: created tier {tier!r} should appear in §1 My tiers; "
        f"error={subs.error_text()!r}"
    )

    # ── The subscriber subscribes headlessly — always queues (no server-side ──
    # mint; the design call this test was restructured under).
    sub_actor = ApiActor(
        subs_consumer_nest["url"], subscriber["token"], subscriber["actor_id_hex"],
        bytes(subscriber["signing_key"]),
    )
    reply = sub_actor.subscribe(author["actor_id_bytes"], tier)
    assert reply.get("outcome") == "queued", (
        f"subscribing to a fresh (client-minted) tier should queue, got {reply!r}"
    )

    # ── The author approves through the UI → transparent mint + upload; then ──
    # confirm the grant API-side (a deadline poll, testing.md point 14) before
    # the subscriber ever logs in, so the consumer page below reads a settled
    # state on first render.
    subs.refresh()
    assert subs.wait_for_pending_request(1), (
        f"seeding: the queued subscribe should surface in §2 Pending requests; "
        f"error={subs.error_text()!r}"
    )
    subs.approve_first_request()
    deadline = time.time() + AUTHOR_GRANT_BUDGET_S
    granted = None
    while time.time() < deadline:
        status = sub_actor.subscription_status(author["actor_id_bytes"])
        if status.get("tier") == tier:
            granted = status
            break
        time.sleep(2)  # sleep-ok: poll interval of a deadline loop
    assert granted is not None, (
        f"seeding: the approved subscribe never became active within "
        f"{AUTHOR_GRANT_BUDGET_S}s — the author-side mint+upload chain is the "
        f"suspect (test_subscription_approve_grants_keyblob pins it per app); "
        f"author error={subs.error_text()!r}"
    )

    # ── The app switches to the subscriber and opens the consumer page. ────────
    _login_as(app, subs_consumer_nest, subscriber, handle="e2e-subscriber")
    subs = app.subscriptions
    subs.navigate_settings()
    assert subs.wait_for_mine_subscription(tier), (
        f"the subscriber's consumer page should list the active subscription to "
        f"tier {tier!r} (fauna.subscriptions.mine.list); error={subs.error_text()!r}"
    )
    assert subs.mine_count() == 1, "exactly the one active subscription"
    assert subs.mine_status(0) == "active", "the granted subscription reads active"
    # The author was admin-provisioned (no handle set), so the consumer row falls
    # back to the creator's hex actor id — exercising the empty-handle→hex path.
    assert subs.mine_author(0) == author["actor_id_hex"], (
        f"creator column should fall back to the author's actor id, got {subs.mine_author(0)!r}"
    )

    # ── Unsubscribe via the UI → the leave QUEUES (client-minted tier: the ──
    # subscriber stays until the author's client commits the removal —
    # monetization.md § Pillar 1, the unsubscribe-commit rule). The row must
    # honestly persist: an app that optimistically drops it renders a leave as
    # done while the KeyBlob still covers the leaver.
    subs.unsubscribe_first()
    assert subs.error_text() == "", (
        f"queueing the unsubscribe should not surface an error: {subs.error_text()!r}"
    )
    assert subs.mine_count() == 1, (
        "after a queued unsubscribe the row honestly persists until the author "
        "client commits the removal"
    )

    # ── The author's client comes online once more → its connect-time ─────────
    # reconcile commits the queued leave via the removal rotation, with no
    # judgment (a leave is not the author's to refuse). Confirm API-side.
    _login_as(app, subs_consumer_nest, author, handle="")
    deadline = time.time() + AUTHOR_GRANT_BUDGET_S
    remaining = None
    while time.time() < deadline:
        status = sub_actor.subscription_status(author["actor_id_bytes"])
        if status.get("tier") != tier:
            remaining = status
            break
        time.sleep(2)  # sleep-ok: poll interval of a deadline loop
    assert remaining is not None, (
        f"the queued unsubscribe was never committed within "
        f"{AUTHOR_GRANT_BUDGET_S}s — the author pump's unsubscribe-commit leg "
        f"(drain_auto_approvals routing kind='unsubscribe' into the removal "
        f"rotation) is the suspect; author error={subs.error_text()!r}"
    )

    # ── Back as the subscriber: the consumer list is empty. ──
    _login_as(app, subs_consumer_nest, subscriber, handle="e2e-subscriber")
    subs = app.subscriptions
    subs.navigate_settings()
    assert subs.wait_for_mine_empty(), (
        f"after the committed unsubscribe the consumer list should be empty; "
        f"count={subs.mine_count()}, error={subs.error_text()!r}"
    )
    assert subs.error_text() == "", f"no error expected: {subs.error_text()!r}"
