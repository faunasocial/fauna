"""The identities a whole RUN shares — and the acts that must never touch them.

Two acts, two fences here: identity succession (below), and a harness write of
a shared identity's recipient seal key (``refuse_seal_key_write_on_shared_identity``,
called from every WS-RPC client's ``call``).

``test_user`` is ``scope="session"``: one registered identity backs
``logged_in_app`` and everything derived from it, for every test in the run. The
session nest's ``admin`` is shared the same way. Both are perfectly safe to
*read*, *write through*, and *accumulate state on* — that is what they are for.

The first fatal act is **identity succession**. The ceremony re-points the
account to a freshly minted successor and, inside the same nest transaction,
revokes every bearer of the predecessor (`actions/settings.py`
``succeed_identity_with_held_kit``). Run against a shared identity, every LATER
test's login is then refused — the app routes to the identity-import flow
instead of coming online, and the login seam's connection barrier
(`helpers/connection.py`) burns its full 60 s before erroring. The run does not
recover, and no failure message names the test that did it.

Measured, on 2026-08-30: one such test at outcome 467 of a 3 347-test docker
sweep killed every barrier-exposed module from outcome 492 to the end of the
run — 65-92 % of them, for fourteen hours. Modules that never log in stayed
green throughout, which is why the pattern read as a load spike for three
sessions of bisection.

Two fences, deliberately different in kind
------------------------------------------
1. ``tests/test_no_succession_on_the_shared_identity.py`` — a tier_1 source scan
   that refuses the *authoring* shape (a test taking a shared-identity fixture
   and running a ceremony) at collection time, on every app, in every run. That
   is the primary fence: it fires before anything executes.
2. This module — the runtime backstop, for the paths a source scan cannot see:
   a test that points the app at a shared identity through a raw
   ``set_state`` patch rather than through a fixture.

Neither replaces the other, and both are cheap.
"""

from __future__ import annotations

# Actor ids (lowercase hex) that more than one test depends on staying signable.
# Registered by the fixtures that mint them; a set, because the session nest's
# admin is shared for the same reason its `test_user` is.
_SHARED_ACTORS: set[str] = set()


class SharedIdentityCeremony(AssertionError):
    """A succession ceremony was aimed at an identity the whole run shares."""


class SharedIdentityKeyWrite(AssertionError):
    """A harness write aimed at the recipient seal key of a shared identity."""


#: The one wire call that replaces an actor's standing recipient seal key. The
#: account's own app is the only legitimate writer for a shared identity.
SEAL_KEY_WRITE_KIND = "fauna.bridges.provision_recipient_mls_pubkey"


def refuse_seal_key_write_on_shared_identity(kind: str, payload) -> None:
    """Raise before a harness replaces a shared identity's recipient seal key.

    The second act the run cannot survive, after succession
    (``e2e-conventions.md`` convention 10, *A sixth form*). A key written by the
    harness is one the account does not hold. Whatever gets sealed to it is
    counted on that account's conversations page as mail that can never open,
    for the rest of the run, and every later "page shows no error" assertion
    fails on it. A precondition that needs a mailbox enables one through the
    app. A journey that needs a harness-held key uses an actor of its own:
    ``dedicated_actor_app``, a fresh ``_make_user``, or a dedicated nest's admin.

    Called by the WS-RPC client's ``call`` for every request, so it covers every
    harness caller with no per-test discipline. It only reads its arguments, and
    anything but a byte-string ``actor_id`` target is left for the nest to judge.
    """
    if kind != SEAL_KEY_WRITE_KIND or not isinstance(payload, dict):
        return
    target = payload.get("actor_id")
    if not isinstance(target, (bytes, bytearray)):
        return
    target_hex = bytes(target).hex()
    if not is_shared_actor(target_hex):
        return
    raise SharedIdentityKeyWrite(
        f"refusing `{kind}` for {target_hex!r}, an identity the WHOLE RUN shares "
        f"(the session `test_user` or the session nest's admin). The harness does "
        f"not hold the key it would write, so mail sealed to it can never open in "
        f"that account's app, and the unopenable-mail notice then stands on its "
        f"conversations page for the rest of the run. Enable the mailbox through "
        f"the app, or use an actor of your own (`dedicated_actor_app`, a fresh "
        f"`_make_user`, a dedicated nest's admin). See e2e-conventions.md "
        f"convention 10, *A sixth form*."
    )


def remember_shared_actor(actor_id_hex: str | None) -> None:
    """Record an actor whose signability the rest of the run depends on."""
    if actor_id_hex:
        _SHARED_ACTORS.add(actor_id_hex.strip().lower())


def is_shared_actor(actor_id_hex: str | None) -> bool:
    return bool(actor_id_hex) and actor_id_hex.strip().lower() in _SHARED_ACTORS


def shared_actors() -> frozenset[str]:
    """The registered set — for tests of this rule, not for callers."""
    return frozenset(_SHARED_ACTORS)


def _forget_all_for_test() -> None:
    """Drop the registry. Only ``test_no_succession_on_the_shared_identity``."""
    _SHARED_ACTORS.clear()


def refuse_ceremony_on_shared_identity(driver, *, ceremony: str) -> None:
    """Raise before an irreversible ceremony aimed at a shared identity.

    Reads ``session.actor_id`` — the agent's own ``session_override``, i.e. the
    identity the app was last *pointed at*. That is the wrong observable for
    asking "did a switch take effect" (`helpers/e2e_session.py` records exactly
    that trap, which made a barrier vacuous on linux until 2026-08-18) and the
    right one here: the question is which identity this test aimed the ceremony
    at, not which one a live session settled on. If the patch named the shared
    actor, the ceremony is forbidden whether or not the switch landed.

    **Fails open.** An app that publishes no session block, a driver with no
    ``get_state``, or a read that raises leaves the ceremony to proceed: this is
    a backstop behind a source-level fence, and a guard that could refuse a
    legitimate ceremony on a missing observable would be worse than the bug.
    """
    try:
        actor = driver.get_state("session.actor_id")
    except Exception:
        return
    if not isinstance(actor, str) or not is_shared_actor(actor):
        return
    raise SharedIdentityCeremony(
        f"refusing `{ceremony}`: this app is signed in as {actor!r}, an identity "
        f"the WHOLE RUN shares (the session `test_user` or the session nest's "
        f"admin). The ceremony revokes the predecessor's bearers, so it would "
        f"sign out every later test in this run — each one would then burn the "
        f"full connection barrier and ERROR, naming none of this. Log in as a "
        f"DEDICATED actor instead: `ungranted_app` (app only) or "
        f"`succeedable_app` (app plus that actor's own credentials). See "
        f"helpers/shared_identity.py for what this cost on 2026-08-30."
    )
