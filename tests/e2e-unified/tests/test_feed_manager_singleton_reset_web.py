"""Sign-out (feed manager built) then a different identity in the SAME
document — the FEED MANAGER SINGLETON must be rebuilt, not reused.

Sibling of `test_bearer_cache_web.py`, but pins a DIFFERENT bug/mechanism.
`feed.ts`'s `getFeedManager()` singleton wraps a shared-Rust `FeedManager`
(`libs/fauna-feed/src/manager.rs`) whose `actor_secret` is baked in ONCE at
construction and used to sign every `submit_post`. Prior to the fix,
`identity.logout()` never cleared this singleton — `signOut()` `goto`s rather
than reloading, so module state (the `managerPromise`) survives into the next
identity — so a same-page sign-out then sign-in-as-a-different-identity kept
composing through the SIGNED-OUT actor's manager: the post is either signed
with the OLD actor's key (nest attributes it to the wrong author) or the
submit fails outright once `rpc.ts`'s `getClient()` retires the old actor's
WS-RPC client (`retireClient` calls `.close()`) out from under the stale
manager's connection.

This is independent of the bearer-cache fix (`api.ts`'s `tokenCache`
— a different cache, keyed differently, cleared by a different code path):
reverting ONLY that fix still breaks this test, because the leak lives in
the FeedManager's own embedded signing secret, not in an HTTP bearer.

**A must actually BUILD the singleton before signing out**, or the test
proves nothing — `feed/+page.svelte`'s `onMount` is the *only* thing that
calls `getFeedManager()` (confirmed by manual trace),
and the SPA's root route defaults to `/app/conversations`, so a bare
`hard_reload()` alone never visits Feed. A composes through the real UI (not
just a page visit) so the manager is fully built via a real async round trip,
not merely requested.

B is deliberately an UNREGISTERED secret redeemed through the invite wizard
(`test_bearer_cache_web.py`'s proven B-leg via
`navigate_to_invite_request_for_known_nest` — a pure state mutation), not a
pre-registered actor re-entering through handle-check: an already-registered
secret's loopback-shaped re-login resolves its final `nest_url` via
`resolve_handle_domain_with_local_port` (`machine.rs`'s `AlreadyOnNest` arm),
which is **always** `https://` for an explicit port — incompatible with the
plain-HTTP test nest a web browser (single-origin, can't trust a self-signed
cert — see `test_onboarding_self_signed_probe.py`) requires. The invite-wizard
mutator sidesteps that resolution entirely by injecting the already-correct
`nest_url` directly, exactly like `test_bearer_cache_web.py`'s B.

Follows `test_bearer_cache_web.py`'s discriminator style: the nest, not
client-side state, is the witness. `session.actor_id` reads the same
in-memory `identity` store on both sides of the fix and proves nothing.
"""
from __future__ import annotations

import json
import secrets
import time

import pytest
from nacl.signing import SigningKey

from actions import ActionLayer
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import register_handled_actor
from conftest import MAIL_PRIMARY_DOMAIN
from drivers import create_driver
from helpers.connection import ConnectionBarrierTimeout, wait_until_online
from helpers import web_store
from tests.api import ws_api

pytestmark = [pytest.mark.tier_3, pytest.mark.web]


def _text_or_absent(driver, element_id: str) -> str:
    """`get_text` that degrades to a marker instead of raising — only ever
    called from a failure path, where a second exception would replace the
    real diagnosis with a useless one."""
    try:
        return driver.get_text(element_id)
    except Exception:
        return "<absent>"


def _seed_registry_identity(driver, *, secret_hex: str, node_url: str) -> None:
    """Write one signed-in identity in the registry shape `identity.init()`
    reads (`helpers/web_store.seed_identity`)."""
    web_store.seed_identity(driver, secret_hex, nest_url=node_url)


def _mint_invite_code(nest) -> str:
    """Mint a one-use OOB invite code as the nest admin. Idiom:
    `test_bearer_cache_web.py::_mint_invite_code`."""
    admin = nest["admin"]["signing_key"]
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin.verify_key),
        signing_key=bytes(admin),
    ) as ws:
        reply = ws.call("fauna.admin.invite_codes.create", {"tier": "free", "uses": 1})
    code = reply["code"]
    assert code, f"admin invite mint returned no code: {reply!r}"
    return code


def _redeem_invite_to_logged_in(actions, *, nest_url: str, handle: str, code: str) -> None:
    """Drive B's invite page from `InviteRequest` through to the feed — verbatim
    idiom from `test_bearer_cache_web.py::_redeem_invite_to_logged_in` (see there
    for why the pure `navigate_to_invite_request_for_known_nest` mutator is used
    instead of a typed handle-check: it keeps B off the handle→`https://`
    resolution path a plain-HTTP proxy topology cannot serve)."""
    driver = actions.driver
    driver.wait_for("handle-input", timeout=15)
    driver.call_machine_method(
        "navigate_to_invite_request_for_known_nest",
        json.dumps([nest_url, handle]),
    )
    driver.wait_for("invite-code-input", timeout=20)
    driver.type_text("invite-code-input", code)
    driver.click("invite-code-check-button")

    deadline = time.monotonic() + 20.0
    while time.monotonic() < deadline:
        if driver.is_enabled("invite-request-continue-button"):
            break
        time.sleep(0.5)
    else:
        raise AssertionError(
            "the OOB code never validated: `invite-request-continue-button` stayed "
            f"disabled. status: {_text_or_absent(driver, 'invite-code-status')!r}; "
            f"error: {actions.error_text()!r}"
        )

    driver.click("invite-request-continue-button")
    # Redeeming is a JOIN, so it lands on the one-tap trust offer before the
    # app (`onboarding.md` § 3b-ter). Decline — this test's subject is the feed
    # manager's rebuild, which § 3b-ter's "declining leaves everything as
    # today" keeps untouched.
    actions.onboarding.finish_joiner_trust_prompt(grant=False)
    try:
        driver.wait_for("compose-text-field", timeout=30)
    except Exception as e:
        raise AssertionError(
            "B redeemed a valid OOB code but never landed on the feed "
            f"({type(e).__name__}). status: "
            f"{_text_or_absent(driver, 'invite-code-status')!r}; error: "
            f"{actions.error_text()!r}."
        ) from e


def _wait_connected(actions, timeout: float = 90.0) -> None:
    """Wait until the transport is online — the shared connection barrier.
    Firing a post before this hits `not connected` regardless of which actor's
    manager is live.

    Was a local poll of the LOCALIZED ``connection-status`` indicator text for a
    ``"Connected"`` prefix — one of five such copies, none of them shared and
    all of them the wrong polarity: the offline gate is *online unless the word
    is a KNOWN offline word*, so an equality test blocks forever on the first
    future state word, the one case the gate was built to tolerate. The barrier
    reads the app's own published ``connection`` observable instead and takes
    that verdict from shared Rust
    (``helpers/connection.py``; ``fauna_e2e_agent::CONNECTION_KEY``).

    Kept as a local wrapper rather than a bare call because the console tail is
    this test's own diagnosis: the reconnect loop the docstring below describes
    (`wss://` against a plain-HTTP test nest) is only visible there, and the
    barrier cannot know to look.
    """
    try:
        wait_until_online(actions.driver, timeout=timeout)
    except ConnectionBarrierTimeout as e:
        console = "\n".join(actions.driver.console_log()[-30:])
        raise AssertionError(
            f"{e} error: {actions.error_text()!r}. last console lines:\n{console}"
        ) from e


def _wait_post_on_nest(actions, *, port: int, reader: dict, body: str, timeout: float = 45.0):
    """Poll the nest until a post with `body` lands, and return it. The nest is
    the witness — read as `reader` (A, registered from the start) so the read
    itself can never be the thing that fails."""
    deadline = time.monotonic() + timeout
    seen: list = []
    while time.monotonic() < deadline:
        posts = ws_api.local_feed_posts(port, reader)
        for p in posts:
            if p.get("body") == body:
                return p
        seen = [p.get("body") for p in posts]
        time.sleep(1.0)
    raise AssertionError(
        f"B's post never reached the nest within {timeout}s. Looked for {body!r}; "
        f"the nest's local feed holds {seen!r}. compose-error: "
        f"{_text_or_absent(actions.driver, 'compose-error')!r}; error: "
        f"{actions.error_text()!r}.\n"
        "If the manager-singleton fix is reverted, B composes through "
        "A's stale FeedManager — either signed with A's key (the nest attributes "
        "the post to A) or the submit fails outright once `rpc.ts` retires A's "
        "WS-RPC client out from under it (a 'not connected'-shaped compose-error)."
    )


def test_web_sign_out_then_new_identity_rebuilds_feed_manager(
    registration_posture_nest, registration_posture_spa_url
):
    """A builds + uses the FeedManager singleton, signs out; B (a different,
    freshly-registering identity, in the SAME document) logs in and posts —
    the nest must attribute B's post to B, proving `getFeedManager()` was
    rebuilt fresh for B and is not still bound to A's secret.

    Red with the fix reverted: B's post either comes back authored by A,
    or never reaches the nest at all (see `_wait_post_on_nest`'s failure text).
    """
    nest = registration_posture_nest
    spa = registration_posture_spa_url
    port = nest["port"]

    # A: a registered, handled actor whose FeedManager singleton is the one at
    # risk of leaking into B's session.
    actor_a = register_handled_actor(
        port, handle="alice" + secrets.token_hex(3), domain=MAIL_PRIMARY_DOMAIN
    )
    secret_a = bytes(actor_a["signing_key"]).hex()

    # B: a locally-generated keypair, NOT registered yet — redeemed through the
    # invite wizard below (mirrors test_bearer_cache_web.py's B; see module
    # docstring for why B is unregistered rather than pre-registered).
    b_signing_key = SigningKey.generate()
    secret_b = bytes(b_signing_key).hex()
    actor_b_hex = bytes(b_signing_key.verify_key).hex()
    handle_b = "bob" + secrets.token_hex(3) + "@" + MAIL_PRIMARY_DOMAIN

    code = _mint_invite_code(nest)
    post_a_body = "manager-singleton A-probe " + secrets.token_hex(4)
    post_b_body = "manager-singleton B-probe " + secrets.token_hex(4)

    driver = create_driver("web")
    driver.launch({"url": spa + "/app/"})
    try:
        actions = ActionLayer(driver)

        # ── A signs in and BUILDS the FeedManager singleton. Visiting Feed is
        #    the only thing that calls getFeedManager() (feed/+page.svelte's
        #    onMount) — the SPA's root route defaults to /app/conversations, so
        #    a bare hard_reload never constructs it on its own. ──
        _seed_registry_identity(driver, secret_hex=secret_a, node_url=spa)
        driver.hard_reload()
        driver.wait_for("feed-tab", timeout=30)
        driver.click("feed-tab")
        _wait_connected(actions)
        actions.feed.create_post(post_a_body)

        # ── Sign out through the real UI: client-side `goto`, no reload — so
        #    feed.ts's module-level `manager` / `managerPromise` survive unless
        #    `identity.logout()` resets them. Lands on `create-identity-button`,
        #    exactly where B's import starts. ──
        actions.settings.sign_out()

        # ── B imports its unregistered key and registers via the invite wizard ──
        actions.onboarding.import_key(secret_b)
        _redeem_invite_to_logged_in(actions, nest_url=spa, handle=handle_b, code=code)

        # ── B posts through the real composer ──
        driver.wait_for("compose-text-field", timeout=30)
        _wait_connected(actions)
        driver.type_text("compose-text-field", post_b_body)
        driver.click("post-submit-button")

        # ── The assertion: who does the NEST think wrote B's post? ──
        post = _wait_post_on_nest(actions, port=port, reader=actor_a, body=post_b_body)
        author = post["author"]
        assert author == actor_b_hex, (
            f"the nest attributed B's post to {author!r}, but B is {actor_b_hex!r}. "
            f"A is {actor_a['actor_id_hex']!r} — if that is the author, B composed "
            "through A's stale FeedManager singleton (signed with A's secret). "
            "`identity.logout()` must reset the feed manager singleton "
            "(`resetFeedManager()`, `apps/fauna-web/src/lib/feed.ts`)."
        )
    finally:
        driver.teardown()


def test_web_add_account_rebuilds_feed_manager(
    registration_posture_nest, registration_posture_spa_url
):
    """The SECOND door of the same manager-singleton-leak class: `identity.logout()` was fixed (the test
    above) to reset the feed/conversations manager singletons, but the append-mode
    "Add account" flow never goes through `logout()` — it calls `identity.login()`
    directly (`onboarding/+page.svelte`'s `importIdentity()`), which was NOT given
    the same reset. So A signs in and builds the FeedManager singleton, then — with
    NO sign-out — adds a second identity B via Settings → Account → Add account →
    Import → invite-wizard redemption; B's completed append soft-navs to
    `/app/feed` (`replaceState`, no reload; onboarding's `LoggedIn` handler, append
    branch) and composes through whichever manager `getFeedManager()` returns.

    B is redeemed through the invite wizard exactly like the sign-out-door test
    above (`_mint_invite_code` + `_redeem_invite_to_logged_in`), NOT via a
    synthetic `AlreadyOnNest` handle-check snapshot for a `user@localhost:PORT`
    handle: that shape drives the wizard's real `LoggedIn` handler to resolve
    B's `nest_url` via `resolve_handle_domain_with_local_port`, which is
    unconditionally `https://` for an explicit-port loopback handle — the same
    trap documented in a ledger entry (confirmed by hand: that recipe hangs
    `_wait_connected` on a real `wss://…` `ERR_SSL_PROTOCOL_ERROR` reconnect
    loop against this plain-HTTP test nest, visible via `driver.console_log()`).
    The invite-wizard mutator sidesteps this entirely by injecting the
    already-correct `nest_url` directly, exactly like `test_bearer_cache_web.py`.

    Red before the `identity.login()` fix: B's post comes back authored by A (or
    the compose fails once `rpc.ts` retires A's WS-RPC client out from under the
    stale manager), the identical failure shape `_wait_post_on_nest` documents
    above for the sign-out door.
    """
    nest = registration_posture_nest
    spa = registration_posture_spa_url
    port = nest["port"]

    # A: a registered, handled actor whose FeedManager singleton is the one at
    # risk of leaking into B's session (same idiom as the sign-out-door test).
    actor_a = register_handled_actor(
        port, handle="addacct-a" + secrets.token_hex(3), domain=MAIL_PRIMARY_DOMAIN
    )
    secret_a = bytes(actor_a["signing_key"]).hex()

    # B: a locally-generated keypair, NOT registered yet — redeemed through the
    # invite wizard below, exactly like the sign-out-door test's B.
    b_signing_key = SigningKey.generate()
    secret_b = bytes(b_signing_key).hex()
    actor_b_hex = bytes(b_signing_key.verify_key).hex()
    handle_b = "addacct-b" + secrets.token_hex(3) + "@" + MAIL_PRIMARY_DOMAIN

    code = _mint_invite_code(nest)
    post_a_body = "add-account manager-singleton A-probe " + secrets.token_hex(4)
    post_b_body = "add-account manager-singleton B-probe " + secrets.token_hex(4)

    driver = create_driver("web")
    driver.launch({"url": spa + "/app/"})
    try:
        actions = ActionLayer(driver)

        # ── A signs in and BUILDS the FeedManager singleton (same recipe as the
        #    sign-out-door test above: visiting Feed + composing is the only way
        #    `getFeedManager()` gets called). ──
        _seed_registry_identity(driver, secret_hex=secret_a, node_url=spa)
        driver.hard_reload()
        driver.wait_for("feed-tab", timeout=30)
        driver.click("feed-tab")
        _wait_connected(actions)
        actions.feed.create_post(post_a_body)

        # ── Add account (NO sign-out): Settings → Account → Add account lands on
        #    the SAME identity_choice step a fresh wizard starts at (`appendMode`
        #    just changes what happens on success — `onboarding/+page.svelte`'s
        #    `?add=1` branch: `resetMachine()` + a clean IdentityChoice). ──
        driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
        })
        driver.wait_for("account-add-button", timeout=15)
        driver.click("account-add-button")

        # ── B imports its unregistered key and registers via the invite wizard —
        #    verbatim the sign-out-door test's B-leg, just with no prior sign-out. ──
        actions.onboarding.import_key(secret_b)
        _redeem_invite_to_logged_in(actions, nest_url=spa, handle=handle_b, code=code)

        # ── B posts through the real composer. ──
        driver.wait_for("compose-text-field", timeout=30)
        _wait_connected(actions)
        driver.type_text("compose-text-field", post_b_body)
        driver.click("post-submit-button")

        # ── The assertion: who does the NEST think wrote B's post? ──
        post = _wait_post_on_nest(actions, port=port, reader=actor_a, body=post_b_body)
        author = post["author"]
        assert author == actor_b_hex, (
            f"the nest attributed B's post to {author!r}, but B is {actor_b_hex!r}. "
            f"A is {actor_a['actor_id_hex']!r} — if that is the author, the "
            "add-account append composed through A's stale FeedManager singleton. "
            "`identity.login()` must reset the feed manager singleton too "
            "(`resetFeedManager()`, `apps/fauna-web/src/lib/feed.ts`), mirroring "
            "`identity.logout()`'s fix."
        )
    finally:
        driver.teardown()
