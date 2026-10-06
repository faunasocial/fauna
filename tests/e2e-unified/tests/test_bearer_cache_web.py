"""Sign-out drops the bearer, so the next identity talks to the nest as itself.

`long-term-store.md` § Cleanup contract, **property 3**: no credential *derived*
from an identity outlives it — the bearer cache is keyed on `(nest, identity)`,
never the nest alone.

**The bug** (bug 2 of 2 — it shipped with zero coverage on any
surface, and this module is the first). `getAuthToken` cached bearers keyed on
the nest URL **alone**, with no identity in the key and no clear on sign-out.
`signOut()` `goto`s rather than reloading, so module state — `tokenCache`
included — survives into the next identity. A user who signed out and then
signed in as a *different* identity **in the same document** was handed the
signed-out actor's bearer, and the SPA talked to the nest **as that account**.

That change shipped **two independent** fixes, and either one alone defeats the
bug (defence in depth) — so a red-green on this module must revert **both**:

1. `api.ts` `tokenKey(base, secretHex)` — the identity is in the key (NUL-joined;
   NUL occurs in neither a URL nor a hex secret, so no pair can collide).
2. `store.ts` `logout()` calls `clearTokenCache()` synchronously, ahead of its
   first `await`.

Reverting only (1) leaves the test **green**, because (2) empties the cache at
sign-out and B mints its own bearer no matter how the key is shaped.

## Why the drive looks the way it does

**B must be UNREGISTERED when its launch silent-challenge runs.** This is the
trap that silently turns the test green while proving nothing. The launch
machine reaches `Online` *only* through a successful silent challenge, and the
`Online` arm calls `primeTokenCache(nestUrl, secret, bearer, ...)`
(`routes/onboarding/+page.svelte`) — which pre-fix keys on the nest alone, so an
**already-registered** B would overwrite A's cached bearer with its own and mask
the bug completely. Unregistered, B's challenge fails, nothing is primed, and
A's bearer stays in the cache to be leaked. B then registers *through the
wizard* (`fauna.account.register` is signature-authed, not bearer-authed, so it
needs no bearer of its own) and the wizard's `LoggedIn` arm only `goto`s to the
feed — it primes nothing. B's first feed request is therefore the first thing to
call `getAuthToken(B)`, which is exactly where the leak lives.

(`silentSignIn` — the other cache writer — cannot fire for B at all: `store.ts`'s
`refreshFromServer` single-flight guard `refreshInFlight` is set by A's boot and
is never reset, so B's `init()` gets A's already-resolved promise back. Trap 1
rides on `primeTokenCache`, not on `silentSignIn`.)

**`session.actor_id` cannot be the assertion.** It reads the in-memory identity
store, which says "B" on *both* sides of the fix. The discriminator has to be
**nest-attributed**: B posts through the real composer, and the author is read
back off the nest (`fauna.feed.local.posts`). The contract is *"after sign-out, a
new identity transacts with the nest as ITSELF"*, and a leaked bearer breaks it
in either of two ways — the test catches both:

* the nest attributes B's post to **A** (the author assertion at the bottom), or
* B never gets a usable session at all, because the bearer it was handed names
  the wrong actor (`_wait_post_on_nest` fires).

⚠ **Recorded red-green (2026-07-17), so the next session doesn't misread a
failure.** With BOTH hunks reverted the observed red is the *second* form:
`compose-error: 'Failed to post: not connected'`, and the nest's local feed stays
empty — B is handed A's bearer, the WS-RPC session never becomes usable (the
transport re-mints and backs off on 4401, `fauna-rpc-wasm/src/client.rs`), and
nothing is ever posted. The author assertion is the sharper statement of the
contract and is kept as the primary one, but it is NOT the arm that fired: on
current main the test goes green through it, and reverted it never gets that far.
So a `_wait_post_on_nest` failure here means *the bug is back*, not that the
harness broke — treat it as a real red and check `api.ts`'s `tokenKey` +
`store.ts`'s `logout()` before suspecting the test.

Reverting `tokenKey` ALONE stays green (see above), so a red-green must revert
both hunks; and the SPA must be rebuilt (`just web-test`) after either revert or
the browser runs the old bundle and the result is meaningless.

**Why `registration_posture_nest` and not the shared `nest_instance`.** B has to
actually register, and `register_core` resolves a fresh nest's posture to
`closed` (`RegistrationClosed`, before it ever looks at an invite code) — the
session `nest_instance` is registration-closed by design. `registration_posture_nest`
is function-scoped and has its registration posture opened post-claim (⇒ `[nest]
registration_mode = "open"`), with a `handle_domain` so the registration
signature over `actor_id || handle || domain` matches what the nest recomputes.
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
    """`get_text` that degrades to a marker instead of raising.

    Only ever called from a failure path: `get_text` raises when the element is
    absent, and an exception thrown while *building* an assertion message
    replaces the real diagnosis with a useless one.
    """
    try:
        return driver.get_text(element_id)
    except Exception:
        return "<absent>"


def _seed_registry_identity(driver, *, secret_hex: str, node_url: str) -> None:
    """Write one signed-in identity in the registry shape `identity.init()`
    reads (`helpers/web_store.seed_identity`); the nest URL rides along so
    the boot silent sign-in has somewhere to go."""
    web_store.seed_identity(driver, secret_hex, nest_url=node_url)


def _wait_bearer_minted(driver, timeout: float = 40.0) -> None:
    """Block until A's boot has actually minted and cached a bearer.

    `refreshFromServer` writes the active account's cached handle into its
    registry index row (`fauna/index`) from the *verify reply* only after
    `silentSignIn` succeeded — and `silentSignIn` caches the bearer on exactly
    that success path. So the appearance of that handle in the index is the
    observable proof that A's bearer is in `tokenCache`.

    Waiting is load-bearing, not hygiene: sign out before A's refresh has run and
    there is no cached bearer to leak, so the whole test would pass vacuously
    (and, worse, would pass with the product reverted).
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        idx = web_store.read_index(driver)
        active = idx.get("active") if idx else None
        row = next(
            (a for a in (idx.get("accounts") or []) if a.get("actor_id") == active),
            None,
        ) if idx else None
        if row and row.get("handle"):
            return
        time.sleep(0.5)
    raise AssertionError(
        f"A's boot never completed its silent sign-in within {timeout}s "
        "(the active account's `fauna/index` row never got a handle), so no "
        "bearer was ever cached and this test would pass vacuously — there "
        "would be nothing for sign-out to leak into the next identity."
    )


def _mint_invite_code(nest) -> str:
    """Mint a one-use OOB invite code as the nest admin.

    `WsRpcAdminClient` has no `create_invite_code` helper — the generic `.call()`
    is the seam (prior art: `tests/platform/docker/test_mail_family_gate_docker.py`).
    `nest["admin"]["signing_key"]` is a PyNaCl `SigningKey` object, not bytes.
    """
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
    """Drive B's invite page from `InviteRequest` through to the feed.

    `navigate_to_invite_request_for_known_nest` is the *production* routing for
    exactly B's state — `machine.rs`: "when the silent-challenge handshake
    reports the secret is unregistered on an otherwise-running nest, the user
    needs an invite — drop them directly on the invite-request page rather than
    make them re-type their handle". It is a pure state mutation (step +
    nest_url + handle), which is also what keeps B off the handle→`https://`
    resolution path that the plain-HTTP proxy topology cannot serve.

    The invite step has no action-layer methods, so it is driven raw; the idiom
    (including the `handle-input` wait that proves the machine is live before
    calling into it) is `test_family.py`'s. `call_machine_method` takes a
    positional JSON list and has no `timeout` kwarg on web.

    No test in the tree had previously redeemed an OOB code through the UI to
    `LoggedIn` — the two in-tree `invite-request-continue-button` clicks are the
    `PendingReview→InviteSubmitted` exit, not the `oob-code-valid→Done` redeem —
    so this leg is new ground and diagnoses itself loudly.
    """
    driver = actions.driver
    driver.wait_for("handle-input", timeout=15)
    driver.call_machine_method(
        "navigate_to_invite_request_for_known_nest",
        json.dumps([nest_url, handle]),
    )
    driver.wait_for("invite-code-input", timeout=20)
    driver.type_text("invite-code-input", code)
    driver.click("invite-code-check-button")

    # Continue is gated on `oob-code-valid` (ui.yaml `invite_request`), so the
    # check has to land before the click — clicking a still-disabled button is a
    # silent no-op that would surface only as a confusing timeout further down.
    # `is_enabled` is False for an absent element too, so this waits for the gate
    # itself (prior art: `test_handle_entry_outcomes.py` over the outcome matrix).
    deadline = time.monotonic() + 20.0
    while time.monotonic() < deadline:
        if driver.is_enabled("invite-request-continue-button"):
            break
        time.sleep(0.5)
    else:
        raise AssertionError(
            "the OOB code never validated: `invite-request-continue-button` stayed "
            f"disabled, so `oob-code-valid` was never reached. status: "
            f"{_text_or_absent(driver, 'invite-code-status')!r}; error: "
            f"{actions.error_text()!r}"
        )

    driver.click("invite-request-continue-button")
    # Redeeming is a JOIN, so it lands on the one-tap trust offer before the
    # app (`onboarding.md` § 3b-ter). Decline — this test's subject is the
    # bearer cache, and § 3b-ter promises that declining leaves everything as
    # today.
    actions.onboarding.finish_joiner_trust_prompt(grant=False)
    try:
        driver.wait_for("compose-text-field", timeout=30)
    except Exception as e:
        raise AssertionError(
            "B redeemed a valid OOB code but never landed on the feed "
            f"({type(e).__name__}). status: "
            f"{_text_or_absent(driver, 'invite-code-status')!r}; error: "
            f"{actions.error_text()!r}. The likely cause is the nest refusing the "
            "registration itself — `register_core` rejects a `closed` posture "
            "before it ever looks at the invite code."
        ) from e


def _wait_connected(actions, timeout: float = 90.0) -> None:
    """Wait until B's transport is online — the shared connection barrier.

    The authed shell renders before `start_ws_rpc()`'s async `connect()`
    completes, so firing any WS-RPC before this hits `not connected` — the post
    below rides WS-RPC.

    This does **not** weaken the assertion. Pre-fix, B's connection still comes
    up — on A's leaked bearer, which is the whole point: the nest accepts it and
    attributes what follows to A. Waiting only removes a race that would fail the
    test for a reason unrelated to the bug.

    Was a local poll of the LOCALIZED ``connection-status`` indicator text for a
    ``"Connected"`` prefix — one of five such copies, none of them shared and
    all of them the wrong polarity: the offline gate is *online unless the word
    is a KNOWN offline word*, so an equality test blocks forever on the first
    future state word, the one case the gate was built to tolerate. The barrier
    reads the app's own published ``connection`` observable instead and takes
    that verdict from shared Rust
    (``helpers/connection.py``; ``fauna_e2e_agent::CONNECTION_KEY``).

    Kept as a local wrapper rather than a bare call so the timeout still names
    what THIS test loses by it (conventions point 6): the barrier says which
    word the app was stuck on, and this says why that ends the test.
    """
    try:
        wait_until_online(actions.driver, timeout=timeout)
    except ConnectionBarrierTimeout as e:
        raise AssertionError(
            f"{e} So nothing could be posted and the bearer question is never "
            f"asked. error: {actions.error_text()!r}"
        ) from e


def _post_via_composer(actions, body: str) -> None:
    """Submit a post through the real composer. Does NOT wait for the feed echo.

    `FeedActions.create_post` waits for the post to appear in the *poster's own
    feed*, which is a different question from the one this test asks — who the
    NEST thought wrote it — and one a just-registered identity with no
    subscriptions can answer differently. The nest read is the assertion here, so
    this only has to get the submit away; `_wait_post_on_nest` does the waiting
    against the authority that matters.
    """
    driver = actions.driver
    driver.wait_for("compose-text-field", timeout=30)
    _wait_connected(actions)
    driver.type_text("compose-text-field", body)
    driver.click("post-submit-button")


def _wait_post_on_nest(actions, *, port: int, reader: dict, body: str, timeout: float = 45.0):
    """Poll the nest until B's post lands, and return it. The nest is the witness.

    Read as `reader` (A) rather than B: the question is what the nest recorded,
    and A is registered from the start, so the read itself can never be the thing
    that fails.
    """
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
        "This is the RECORDED red for this test (see the module docstring): with "
        "the per-actor bearer-cache fix reverted, B is handed the signed-out actor's bearer, its "
        "WS-RPC session never becomes usable ('Failed to post: not connected'), "
        "and nothing is posted. Suspect the product first — `api.ts` `tokenKey` "
        "must key on (node, secret) and `store.ts` `logout()` must call "
        "`clearTokenCache()` — before suspecting this test."
    )


@pytest.mark.feature("account")
def test_web_sign_out_then_new_identity_posts_as_itself(
    registration_posture_nest, registration_posture_spa_url
):
    """A signs out; B signs in in the same document and posts **as B**.

    Red with both hunks reverted: the nest attributes B's post to A,
    because B was handed A's cached bearer.
    """
    nest = registration_posture_nest
    spa = registration_posture_spa_url
    port = nest["port"]

    # A: a registered, handled actor whose bearer will be the one at risk of leaking.
    actor_a = register_handled_actor(
        port, handle="alice" + secrets.token_hex(3), domain=MAIL_PRIMARY_DOMAIN
    )
    secret_a = bytes(actor_a["signing_key"]).hex()

    # B: a locally-generated keypair that is NOT registered (the trap — see the
    # module docstring). `create_actor_and_register` would register it and mask
    # the bug; the wizard registers it below, after the silent challenge has
    # already failed.
    b_signing_key = SigningKey.generate()
    secret_b = bytes(b_signing_key).hex()
    actor_b_hex = bytes(b_signing_key.verify_key).hex()
    # The wizard's handle format is `user@domain` (cf. `test_onboarding_localhost.py`'s
    # `test@localhost:{port}`), and the domain is load-bearing, not decoration: the
    # machine registers the bare local part but signs over the handle's `@domain`,
    # and the nest verifies against its OWN configured handle domain. A bare handle
    # here would sign over "" and be refused as `signature_failed`.
    handle_b = "bob" + secrets.token_hex(3) + "@" + MAIL_PRIMARY_DOMAIN

    code = _mint_invite_code(nest)
    body = "bearer-leak probe " + secrets.token_hex(4)

    driver = create_driver("web")
    driver.launch({"url": spa + "/app/"})
    try:
        # ── A signs in, minting + caching a bearer for this nest ──
        _seed_registry_identity(driver, secret_hex=secret_a, node_url=spa)
        driver.hard_reload()
        _wait_bearer_minted(driver)

        # ── Sign out through the real UI. Client-side `goto`, no reload: module
        #    state (the bearer cache) survives into B unless `logout()` clears it.
        #    Lands on `create-identity-button`, exactly where B's import starts.
        actions = ActionLayer(driver)
        actions.settings.sign_out()

        # ── B imports its unregistered key and registers via the wizard ──
        actions.onboarding.import_key(secret_b)
        _redeem_invite_to_logged_in(actions, nest_url=spa, handle=handle_b, code=code)

        # ── B posts through the real composer ──
        _post_via_composer(actions, body)

        # ── The assertion: who does the NEST think wrote it? ──
        post = _wait_post_on_nest(actions, port=port, reader=actor_a, body=body)
        author = post["author"]
        assert author == actor_b_hex, (
            f"the nest attributed B's post to {author!r}, but B is {actor_b_hex!r}. "
            f"A is {actor_a['actor_id_hex']!r} — if that is the author, B was handed "
            "the signed-out actor's cached bearer and the SPA posted AS A. Sign-out "
            "must drop every bearer derived from the identity it signed out "
            "(`long-term-store.md` § Cleanup contract, property 3)."
        )
    finally:
        driver.teardown()
