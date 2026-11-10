"""Signing an org-minted account in and out of a staging box, as its user would.

Shared by the provisioning drive (``tests/live/test_staging_box_provision.py``,
which admits and seeds the accounts — :func:`admit_accounts`,
:func:`seed_accounts`) and the store reviewer journeys
(``helpers/store_reviewer_walk.py``, which walks one as a reviewer: live in
``tests/test_store_reviewer_journey.py``, and in
``tests/test_store_reviewer_journey_local.py`` on a standalone nest seeded
through the same two functions).
Every step is an app gesture (convention 8); every wait a deadline poll on
observable state (convention 14).

Why sign-out and the stale-device sweep belong to every leg: the free tier the
org accounts sit at holds 3 devices, and an instance left signed in keeps a
device slot for good. So every leg ends with the app's Settings sign-out, which
retires its enrollment, and every sign-in first removes, on Settings → Devices,
any device a leg that died before its sign-out left (:func:`clear_stale_devices`).
"""
from __future__ import annotations

import time

import pytest

from helpers.live_admin import wait_connected, wait_logged_in
from helpers.waiting import await_device_removal_ready
from i18n.strings import S

#: This machine's enrollment showing on Settings → Devices — marked, or refused
#: at the cap; after a freed slot, the pump's next pass (its 300 s backstop at
#: worst) and the page re-reading it.
DEVICE_ENROLL_TIMEOUT_S = 420
DEVICE_REMOVE_TIMEOUT_S = 60  # one removal leaving the roster
#: The largest shipped tier holds 10 devices (`page_walk.rs`); more removals
#: than that is a loop, not a backlog.
MAX_STALE_REMOVALS = 12


#: How long a sign-in waits for a remote handle check to enable Continue.
HANDLE_CHECK_S = 120.0


def poll(fn, desc: str, timeout: float, interval: float = 10.0):
    """Deadline-poll ``fn`` (returning ``(ok, observed)``) until ``ok``; the
    failure names what was awaited and the last observation."""
    deadline = time.monotonic() + timeout
    observed = "<never evaluated>"
    while time.monotonic() < deadline:
        ok, observed = fn()
        if ok:
            return observed
        time.sleep(interval)  # sleep-ok: poll interval inside a deadline poll on observable state (convention 14), not a settle-sleep
    pytest.fail(f"{desc} — not observed within {timeout:.0f}s (last: {observed})")


def sign_in_as(app, secret_hex: str, handle: str, who: str, where: str, *,
               check_terms: bool = False) -> None:
    """Real onboarding sign-in with a stored identity: import the seed, type
    the handle, run the handle check, continue. Never claims — every identity
    signed in with here is one the box at ``where`` already knows.

    ``check_terms`` also asserts what a reviewer reads before Continue: the
    terms notice and its link on the handle screen once the check answers
    *Welcome back* (`terms-and-content-guidelines.md` § The decisions,
    item 3 (iv))."""
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(secret_hex)
    ob.fill_handle(handle)
    ob.run_handle_check(timeout=60)
    # `run_handle_check` returns when its own wait ends, answered or not, and the
    # remote check enables Continue when it answers: wait on Continue itself, never
    # one read of it (convention 14).
    poll(
        lambda: (app.driver.is_enabled("handle-entry-continue-button"),
                 f"message={app.driver.get_text('handle-message-area')!r} "
                 f"error={app.error_text()!r}"),
        f"signing in to {where} as {who}: the handle check to enable Continue",
        HANDLE_CHECK_S,
        interval=1.0,
    )
    if check_terms:
        drv = app.driver
        assert drv.is_visible_scrolled("onboarding-terms-notice"), (
            f"signing in as {who}: no terms notice before Continue — "
            f"{drv.diagnose('onboarding-terms-notice')}"
        )
        assert S.onboarding.terms.notice in drv.get_text("onboarding-terms-notice"), (
            f"signing in as {who}: the terms notice reads "
            f"{drv.get_text('onboarding-terms-notice')!r}"
        )
        assert drv.is_visible_scrolled("onboarding-terms-link"), (
            f"signing in as {who}: no terms link beside the notice — "
            f"{drv.diagnose('onboarding-terms-link')}"
        )
    ob.submit_handle()
    if app.driver.is_visible("claim-code-input"):
        pytest.fail(
            f"{where} is recorded as claimed, yet signing in as {who} the wizard "
            f"asks for a claim code. error={app.error_text()!r}"
        )
    wait_logged_in(app)


def this_device_card(app) -> int | None:
    """The index of the ``device-card`` marked as this machine, or ``None``
    while none is (not yet enrolled, or refused at the cap)."""
    drv = app.driver
    for i in range(drv.count("device-card")):
        if drv.is_visible_scrolled("device-this-mark-badge", scope=f"device-card[{i}]"):
            return i
    return None


def device_limit_text() -> str:
    """The device-cap refusal as the launched app words it: a release build's
    own commit's string when the run launched one (the drives against a kept
    box run the release's app, `helpers/release_app.py`), else the tip's."""
    from helpers import release_app
    return (release_app.app_string("error.sync.device_limit_exceeded")
            or S.error.sync.device_limit_exceeded)


def unverified_removal_text() -> str:
    """The removal refusal for a row whose fleet member this replica has not
    verified yet, as the launched app words it (see :func:`device_limit_text`)."""
    from helpers import release_app
    return (release_app.app_string("devices.error_remove_unverified_device")
            or S.devices.error_remove_unverified_device)


def read_devices(app) -> dict:
    """One fresh visit to Settings → Devices, by clicks (tui hydrates the
    roster on the nav edge, so every read re-enters the page): which card is
    this machine's, which are not, and whether the page states the cap."""
    app.settings.open_devices_by_click()
    drv = app.driver
    this = this_device_card(app)
    notice = (drv.get_text("error-message") or "") if drv.is_visible("error-message") else ""
    return {"this": this,
            "stale": [i for i in range(drv.count("device-card")) if i != this],
            "capped": device_limit_text() in notice,
            "notice": notice}


def _page_notice(app) -> str:
    drv = app.driver
    return (drv.get_text("error-message") or "") if drv.is_visible("error-message") else ""


def _remove_one_stale(app, seen: dict) -> bool:
    """Take one stale row off Settings → Devices; ``False`` when none left
    within :data:`DEVICE_REMOVE_TIMEOUT_S`.

    A removal refused as unverified (`devices.error_remove_unverified_device`:
    this replica has not verified the row's fleet member) is retried on the
    NEXT stale row in turn: the refusal is per row — a row can still be
    genuinely unverifiable. The pre-build record, measured 2026-11-09 on
    dev.fauna.build before a cap-refused pass listed the fleet scope over the
    app session: a cap-refused machine removed a principal-less row and
    refused one carrying a principal on every one of 22 clicks; once a slot
    freed, this machine enrolled ~30 s later. Since that build the capped
    machine verifies a dead enrolled seat and removes it. The refusal is read
    before the re-visit that clears it."""
    before = len(seen["stale"])
    deadline = time.monotonic() + DEVICE_REMOVE_TIMEOUT_S
    clicks, refused = 0, True
    while True:
        if refused and seen["stale"]:
            await_device_removal_ready(app.driver)
            app.driver.click("device-remove-button",
                             index=seen["stale"][clicks % len(seen["stale"])])
            clicks += 1
        time.sleep(2.0)  # sleep-ok: poll interval inside a deadline poll on observable state (convention 14)
        refused = unverified_removal_text() in _page_notice(app)
        seen.update(read_devices(app))
        seen["clicks"] = clicks
        if len(seen["stale"]) < before:
            return True
        if time.monotonic() >= deadline:
            return False


def clear_stale_devices(app, who: str) -> None:
    """Right after a sign-in, before anything needs the account store: remove,
    on Settings → Devices, every device of this account but this machine.

    What keeps a re-run device-neutral. Every leg signs out at its end
    (:func:`sign_out`), which retires its enrollment but leaves the machine's
    named device row on the roster (`sync-agent-credentials.md`, the
    2026-09-20 named-row ruling). A sign-in from the same instance comes back
    to that row, so a second leg for an account removes nothing. Each run is a
    fresh instance, though, and registers a new row, so every run removes the
    one the run before it left (measured 2026-10-23: five resumes in a row,
    each "removed 1"). A leg that died before its sign-out leaves one more row.
    Past the cap the nest refuses this machine's register, the account store
    never comes up, and the leg dies at its next step (2026-10-23). A refused
    machine's pass still serves a removal — the remedy the refusal names
    (`account-client-lifecycle.md` § the cap-refused pass). Removing the device
    in hand is refused by the product (`devices.md` § Removing a Device), so
    this first waits until this machine is marked or the page states the cap
    (then it holds no row, and every card is stale), and ends once this
    machine is marked. Every identity signed in with here is org-minted and
    held only in the dev host's state files, so a device other than this one
    is a leftover run's."""
    seen: dict = {}

    def _settled():
        seen.update(read_devices(app))
        return seen["this"] is not None or seen["capped"], f"{seen} error={app.error_text()!r}"

    poll(_settled, f"{who}: this machine marked on Settings → Devices, or refused at the "
         "device cap", DEVICE_ENROLL_TIMEOUT_S, interval=2.0)
    removed = 0
    while seen["stale"]:
        assert removed < MAX_STALE_REMOVALS, (
            f"{who}: still {len(seen['stale'])} stale devices after removing {removed}: {seen}"
        )
        if _remove_one_stale(app, seen):
            removed += 1
            continue
        if seen["this"] is None:
            pytest.fail(f"{who}: no stale device left the roster within "
                        f"{DEVICE_REMOVE_TIMEOUT_S:.0f}s while this machine is refused at "
                        f"the cap (last: {seen} error={app.error_text()!r})")
        # Enrolled, so the leg can go on; what is left is refused from here.
        print(f"[org-account] {who}: {len(seen['stale'])} stale device(s) refused as "
              f"unverified, left on the roster")
        break
    if seen["this"] is None:
        # The freed slot is taken by the pump's next pass, on its own cadence.
        def _marked():
            seen.update(read_devices(app))
            return seen["this"] is not None, f"{seen} error={app.error_text()!r}"

        poll(_marked, f"{who}: this machine enrolled once the cap had room",
             DEVICE_ENROLL_TIMEOUT_S, interval=2.0)
    app.settings.leave_sub_page_by_click()
    print(f"[org-account] {who}: removed {removed} stale device(s); this machine enrolled")


#: The words the app's account store refuses a read with while it is not
#: ready — the first listing still owed, or the unkeyed hold standing
#: (`account-client-lifecycle.md` § The first listing, clauses (3) and (5)).
#: Rust's `LEDGER_NOT_READY` (`libs/fauna-client-config/src/store_seam.rs`),
#: which the release's app spells the same way.
STORE_NOT_READY_TEXT = "the account store is not ready yet"
#: A store held for the escrow holder's answer clears at the pass that asks,
#: and the pump's backstop pass comes at most 300 s apart: one full backstop
#: and the page re-reading it.
STORE_READY_TIMEOUT_S = 420


def store_not_ready(error_text: str | None) -> bool:
    """Whether the page's error says the account store refused its read as not
    ready — a gesture refused that way is retried, never awaited: the refused
    write is gone, and polling its effect could never succeed."""
    return STORE_NOT_READY_TEXT in (error_text or "")


def store_hold_lines(log_text: str, limit: int = 12) -> list[str]:
    """The app log's own account of the store's hold — the read gate's verdict
    lines (which sources stand: device, holder, sibling) and the escrow
    recovery's — first and last kept, so a failure says what the store waited
    on even when the report's tail has rolled past the start."""
    marks = ("unkeyed hold", "escrow", "still held reads")
    hits = [ln for ln in log_text.splitlines() if any(m in ln for m in marks)]
    if len(hits) <= limit:
        return hits
    half = limit // 2
    return [*hits[:half], f"<{len(hits) - limit} more>", *hits[-half:]]


def sign_out(app) -> None:
    """End a signed-in leg the way its user would: Settings sign-out, which
    retires this machine's enrollment nest-side (`fleet-scope-reclamation.md`
    clause (4)). The machine's named row outlives it, and the next run's
    :func:`clear_stale_devices` removes that row."""
    try:
        app.settings.sign_out_by_click()
    except TimeoutError as e:
        # e2e rule 6: say what the app shows instead of onboarding — the
        # top-level `<id> (n/m visible)` lines of the driver tree with n > 0.
        shown = [ln.split(" (")[0] for ln in app.driver.tree().splitlines()
                 if " (" in ln and not ln.startswith(" ") and " (0/" not in ln]
        raise TimeoutError(f"{e}; error={app.error_text()!r} shown={shown}") from e
    print("[org-account] signed out")


def sign_in_fresh(app, account: dict, where: str, *, signed_in: bool = True,
                  check_terms: bool = False) -> None:
    """Sign out the leg before (when ``signed_in``), return the app to factory
    state, then sign in as an org-minted account — a fresh app instance, as a
    reviewer meets it. ``check_terms``: :func:`sign_in_as`."""
    if signed_in:
        sign_out(app)
    app.driver.reset()
    sign_in_as(app, account["secret_hex"], f"{account['localpart']}@{where}",
               account["localpart"], where, check_terms=check_terms)
    wait_connected(app)
    clear_stale_devices(app, account["localpart"])


# ── Admitting and seeding the org accounts ───────────────────────────────────
# One code path for the staging-box provisioning drive (which seeds a kept
# box) and the local store reviewer journey (which seeds a standalone nest the
# same way), so the first run a local reviewer meets is the one a store
# reviewer meets (`org-nests.md` § fauna.build → *Accounts*).

#: One admission, lookup, knock or post landing on the nest.
ACCOUNT_TIMEOUT_S = 120
#: One message send showing (or refusing) on the page.
MESSAGE_TIMEOUT_S = 120


def admit_accounts(app, accounts: dict[str, dict]) -> list[str]:
    """As the nest's admin: admit each account's public key under its handle at
    the free tier, through Admin → Users → Admit. An account already admitted
    (a rerun) is left as it is. An account may carry its seed (the drive's
    org accounts) or only its public key (``actor_hex`` — a witness host's
    tester from the tracked registry, `staging_box.witness_testers_for`).
    Returns the local parts this call had to admit, in order."""
    from helpers.staging_box import account_actor

    admitted: list[str] = []
    app.admin.navigate_users()
    for localpart, account in accounts.items():
        actor = account_actor(account)
        if app.admin.has_user_row(actor):
            print(f"[org-account] {localpart}: already admitted")
            continue
        app.admin.admit_user(actor, handle=localpart, tier="free")

        def _admitted(actor=actor):
            app.admin.navigate_users()
            return app.admin.has_user_row(actor), f"error={app.error_text()!r}"

        poll(_admitted, f"{localpart} ({actor[:12]}…) admitted", ACCOUNT_TIMEOUT_S,
             interval=2.0)
        print(f"[org-account] {localpart}: admitted")
        admitted.append(localpart)
    return admitted


def wait_reachable(app, localpart: str) -> None:
    """Hold a reviewer's session until its own app has published its starter
    keys, read off Settings → Encryption as its owner would see them.

    An admitted account holds no key packages until its own client mints them
    (the private halves never leave the device); the client does so at sign-in,
    after the launch restore (`docs/features/encryption-settings.md` outcome 1,
    `federation.md` § Key packages). Signed out before that, the account cannot
    be added to a conversation: on 2026-10-23 each reviewer was reset about two
    seconds after signing in, and the tester's picker then rightly read
    `not-found` by actor id and, by handle, fell through to the mail rail.

    The publish is the shared receive loop's, which the native apps start
    under e2e only behind the ``real_conversations`` marker's launch gate, so
    every module that seeds through here carries that marker (on 2026-11-05 a
    windows reviewer without it held a count of 0 for the whole budget)."""
    def _stated():
        app.settings.open_encryption(timeout=ACCOUNT_TIMEOUT_S)
        n = app.settings.encryption_keypackage_count()
        return (n or 0) > 0, f"count={n!r} error={app.error_text()!r}"

    poll(_stated, f"{localpart}'s app publishing its starter keys", ACCOUNT_TIMEOUT_S,
         interval=2.0)


def seed_sales(app) -> tuple[str, str]:
    """As the signed-in tester, on a full build: the priced tier
    (``SEED_TIER``) and the post sold on its own (``SEED_SOLD_POST``), each a
    user's gesture — the Tiers tab's create form, compose's "Sell this post…"
    answer. Returns what happened to each, ``created``/``kept`` and
    ``sold``/``kept``, so a rerun on a seeded box adds nothing.

    The store build's buyer half reads these (the iOS store driver's priced-tier
    and sold-post nodes, `docs/features/store-install.md` outcomes 2 and 3); a
    store build cannot make either, its editor having no money fields
    (`dynamic-features.md` § Platform-family surface excision, the
    price-and-route class).

    Idempotent without a fixed wait (convention 14): a tier name is unique per
    author, so the create always runs and resolves into one of two observable
    states — the row painted, or the nest's refusal of a duplicate on the page
    (the form is then cancelled) — and either way the row must show. The sold
    post is looked for on the tester's own Posts tab once the tab has painted
    the plain ``SEED_POSTS``, which every seeding posts first — the read that
    proves the tab loaded."""
    from helpers.staging_box import SEED_POSTS, SEED_SOLD_POST, SEED_TIER

    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    name = SEED_TIER["name"]
    before = list(subs.tier_names())
    tier = "kept" if name in before else None
    if tier is None:
        subs.create_tier(name, price_hint=SEED_TIER["price_hint"],
                         asking_price=SEED_TIER["asking_price"],
                         payment_url=SEED_TIER["payment_url"])
        poll(lambda: (name in subs.tier_names() or app.has_error(),
                      f"tiers={subs.tier_names()} error={app.error_text()!r}"),
             f"the tester's tier {name!r} saved or refused", ACCOUNT_TIMEOUT_S, interval=1.0)
        tier = "created"
        if name not in subs.tier_names():
            # The duplicate refusal: the row exists, the list had not painted it.
            subs.cancel_tier_form()
            tier = "kept"
    poll(lambda: (subs.tier_names().count(name) == 1, f"tiers={subs.tier_names()}"),
         f"the tester's tier {name!r} listed once", ACCOUNT_TIMEOUT_S, interval=1.0)

    def sold_cards() -> list[int]:
        texts = app.profile._post_card_texts_safe()
        return [i for i, t in enumerate(texts)
                if SEED_SOLD_POST["preview"] in t or SEED_SOLD_POST["full_text"] in t]

    def own_posts_loaded():
        app.profile.open_posts_tab()
        texts = app.profile.wait_for_post_cards([SEED_POSTS[-1]], timeout=10.0)
        return any(SEED_POSTS[-1] in t for t in texts), f"cards={texts}"

    subs.navigate()
    poll(own_posts_loaded, "the tester's Posts tab painting the seeded posts",
         ACCOUNT_TIMEOUT_S, interval=1.0)
    sold = "kept" if sold_cards() else None
    if sold is None:
        app.feed.navigate()
        app.feed.sell_post(SEED_SOLD_POST["full_text"], SEED_SOLD_POST["preview"],
                           SEED_SOLD_POST["price"])
        sold = "sold"

    def badged_once():
        subs.navigate()
        app.profile.open_posts_tab()
        cards = sold_cards()
        badges = [app.profile.post_card_badge(i) for i in cards]
        return len(cards) == 1 and all(badges), f"sold cards={cards} badges={badges}"

    poll(badged_once, "the tester's sold post on their Posts tab, once, with its badge",
         ACCOUNT_TIMEOUT_S, interval=2.0)
    print(f"[org-account] tester: tier {name!r} {tier}, sold post {sold}")
    return tier, sold


def seed_accounts(app, accounts: dict[str, dict], where: str, *,
                  signed_in: bool = True, bare_localparts: bool = False,
                  sales: bool | None = None) -> None:
    """Seed the reviewers' first run on the nest at ``where`` (the handle
    domain every account signs in under), every step a user's gesture: the
    tester signs in, asks each reviewer to connect and posts; each reviewer
    signs in, accepts the tester and stays until its own starter keys are
    published; the tester signs in again and opens a conversation with each
    reviewer; then each reviewer signs in again and is shown that conversation
    (`org-nests.md` § fauna.build: a reviewer's first run is not empty).
    ``signed_in`` says whether ``app`` holds a session to sign out first.

    ``bare_localparts`` addresses the other accounts by bare localpart
    (``microsoft-reviewer``) instead of ``localpart@where``. Every account is on
    the one nest, so on a box whose handle domain is ``where`` the two are the
    same gesture; they differ on a loopback test nest, whose ``where`` is the
    host's dial authority (``127.0.0.1:<port>``), a domain the nest does not
    serve. There the app rightly classifies the typed domain as foreign
    (``fauna_core::resolve::is_foreign_handle_domain``) and the nest originates
    ``fauna.federation.inbox.deliver`` to ``https://127.0.0.1:<port>`` — which
    reaches the same nest only by accident in standalone, and reaches nothing
    from inside a container (the docker run of 2026-11-04: the knock never
    arrived). A local test passes True.

    ``sales`` adds the tester's priced tier and sold post (:func:`seed_sales`)
    after the plain posts. ``None`` decides by where: on the two staging boxes
    (``nest_mode.DISPOSABLE_BOX_HOSTS``) yes, anywhere else no — never on the
    acceptance nest ``fauna.build``, whose content is a store reviewer's first
    run, where a payment link buys nothing and no store-build witness reads it.

    The contact request goes tester → reviewer, so each reviewer's accept is a
    gesture of the reviewer's own. The other direction works too since the
    accept writes the requester's edge as well (`contacts.md` § Persistence →
    *The accept makes the edge mutual*, ruled after the 2026-10-23 drive on
    which a reviewer's request, accepted by the tester, left the tester's
    message refused): the reach policy reads the recipient's edge
    (`direct-messages.md` § Reach policy), and either accept now provides it."""
    from helpers.staging_box import SEED_MESSAGE, SEED_POSTS, SEEDING_ACCOUNT, actor_hex_for

    seeder = accounts[SEEDING_ACCOUNT]
    seeder_actor = actor_hex_for(seeder["secret_hex"])
    reviewers = {lp: a for lp, a in accounts.items() if lp != SEEDING_ACCOUNT}

    def address(localpart: str) -> str:
        return localpart if bare_localparts else f"{localpart}@{where}"

    sign_in_fresh(app, seeder, where, signed_in=signed_in)
    for localpart, account in reviewers.items():
        app.contacts.navigate()
        app.contacts.find_by_handle(address(localpart))
        reviewer_actor = actor_hex_for(account["secret_hex"])
        poll(lambda: (app.contacts.actor_id_result_text() == reviewer_actor,
                      f"result={app.contacts.actor_id_result_text()!r} "
                      f"find-error={app.contacts.find_error_text()!r}"),
             f"{SEEDING_ACCOUNT} finding {localpart} by handle", ACCOUNT_TIMEOUT_S,
             interval=2.0)
        app.contacts.add_contact_no_settle()
    app.feed.navigate()
    for text in SEED_POSTS:
        app.feed.create_post(text, timeout=ACCOUNT_TIMEOUT_S)
    print(f"[org-account] {SEEDING_ACCOUNT}: asked {len(reviewers)} reviewers to connect, "
          f"posted {len(SEED_POSTS)}")
    if sales is None:
        from helpers.nest_mode import DISPOSABLE_BOX_HOSTS
        sales = where in DISPOSABLE_BOX_HOSTS
    if sales:
        seed_sales(app)

    def knock_index():
        # The knock row names a sender by short id, or by handle once the
        # roster holds one (`ContactsActions.knock_index_for`), in the form
        # this seeding addresses it.
        return app.contacts.knock_index_for(seeder_actor, address(SEEDING_ACCOUNT))

    def roster():
        try:
            return (f"knocks={app.contacts.knock_senders()} contacts="
                    f"{list(zip(app.contacts.contact_names(), app.contacts.contact_statuses()))} "
                    f"error={app.error_text()!r}")
        except LookupError as e:
            return f"<the page re-rendering mid-read: {e}>"

    def holds_tester() -> bool:
        return any(SEEDING_ACCOUNT in n for n in app.contacts.contact_names())

    def rereading(fn):
        # A list read counts its rows, then reads each one: a re-render
        # between the two (the accept's re-read removing the knock row) is a
        # list still changing, not an answer.
        def _poll():
            try:
                return fn()
            except LookupError as e:
                return False, f"the page re-rendering mid-read: {e}"
        return _poll

    for localpart, account in reviewers.items():
        sign_in_fresh(app, account, where)

        def _knock_or_contact():
            # A rerun on the same server finds the tester already accepted.
            app.contacts.navigate()
            return knock_index() is not None or holds_tester(), roster()

        poll(rereading(_knock_or_contact), f"{localpart} holding {SEEDING_ACCOUNT}'s request",
             ACCOUNT_TIMEOUT_S, interval=2.0)
        index = knock_index()
        if index is not None:
            app.contacts.accept_knock(index)
            poll(rereading(lambda: (knock_index() is None and holds_tester(), roster())),
                 f"{localpart} accepting {SEEDING_ACCOUNT}", ACCOUNT_TIMEOUT_S, interval=1.0)
            how = "accepted"
        else:
            how = "already holds"
        print(f"[org-account] {localpart}: {how} {SEEDING_ACCOUNT} — {roster()}")
        wait_reachable(app, localpart)
        print(f"[org-account] {localpart}: signed in, {how} {SEEDING_ACCOUNT}, reachable")

    sign_in_fresh(app, seeder, where)
    for localpart in reviewers:
        # By handle, as a person types it — reachable because each reviewer
        # waited above for its own starter keys (`wait_reachable`), and
        # admitted because each accepted the tester.
        app.conversations.start_new_conversation(address(localpart), body=SEED_MESSAGE)

        def _sent(localpart=localpart):
            # The send is a round trip: "no error yet" holds the instant after
            # the click, so wait for the thread itself — and read a refusal
            # into the failure while it shows.
            threads = app.conversations.list_threads()
            return (not app.has_error() and any(localpart in t.label for t in threads),
                    f"threads={[(t.label, t.snippet) for t in threads]} "
                    f"error={app.error_text()!r}")

        poll(_sent, f"{SEEDING_ACCOUNT} messaging {localpart}", MESSAGE_TIMEOUT_S, interval=1.0)
    print(f"[org-account] {SEEDING_ACCOUNT}: messaged every reviewer")

    for localpart, account in reviewers.items():
        sign_in_fresh(app, account, where)

        def _seeded():
            app.conversations.navigate()
            return app.driver.count("conversation-item") >= 1, app.error_text()

        poll(_seeded, f"{localpart} seeing {SEEDING_ACCOUNT}'s conversation",
             ACCOUNT_TIMEOUT_S, interval=2.0)
        print(f"[org-account] {localpart}: signed in again and sees the seeded conversation")
