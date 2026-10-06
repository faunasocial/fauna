"""Derive a live box's admin handle from the SECRET, never from out-of-band env.

**The defect this closes.** A `live_box` test signs in with
``FAUNA_LIVE_SECRET_HEX`` and drives the real app UI — and then demanded the
same account's *handle* a second time, as ``FAUNA_LIVE_HANDLE`` (or
``FAUNA_LIVE_MAIL_ADDRESS``). That value lives in no artifact in the repo or on
any dev machine: every docstring and the `justfile` carry ``test@example.com`` as
an *example*, so a session asked to "run the live suite" had to interrupt a
human for a value the box already knows. On 2026-08-16 that alone blocked the
post-release ActivityPub journey against example.com — everything else (the built
linux binary, the located secret, a verified-clean box) was in place. The user's
ruling: *"this detail needs to be a part of the test."*

**Why the box CAN adjudicate** (the reason the ActivityPub-shaped objection does
not hold): a *WebFinger* probe cannot tell a right handle from a wrong one —
with ActivityPub disabled it 404s for every handle alike. But the handle the
test types is not checked by WebFinger; it is checked by the **onboarding
wizard's own handle check**, which runs a key-based silent challenge against the
nest and gets the registered handle back:

  the wizard types ``<probe>@<host>`` into ``handle-input`` → ``run_handle_check``
  fires ``OnboardingMachine::start_handle_check`` →
  ``nest_api.silent_challenge(probe_base, secret)`` returns ``verify.handle``,
  the registered **bare localpart** → the machine completes
  ``HandleCheckOutcome::AlreadyOnNest { handle_matches, current_handle }``
  (`libs/fauna-onboarding-machine/src/machine.rs:2858`) →
  ``submit_handle_check_continue`` publishes
  ``WizardOutcome::LoggedIn { nest_url, handle: current_handle }``
  (`machine.rs:3135`) — **the nest-returned handle wins over the typed one** →
  the linux app's ``AccountLoaded`` / ``IdentityRefreshed`` write it to
  ``AppState.handle`` (`apps/fauna-linux/src/app.rs:2761`, `:2785`) → the test
  agent's state provider serializes it as ``session.handle``
  (`apps/fauna-linux/src/main.rs:4862`) → ``driver.get_state("session.handle")``.

So the derivation is not a new seam bolted onto the product: it is the wizard's
shipped "you're already registered on {domain} as {old_handle}" path, read at
the end instead of guessed at the start. Typing a handle the account does not
own is exactly what a returning user does, and the product already answers it.

**Non-mutating** (`testing.md` § The shared-box rule → *Non-destructive
carve-out*): the silent challenge is a read, ``AlreadyOnNest`` registers
nothing, and the wizard's handle field is not the handle-*change* surface (that
is Settings → ``change-handle``). Signing in mutates no pre-existing data, so a
derivation step may precede a live test's own hard preconditions.

**Where it refuses.** Two cases are deliberately hard failures rather than
guesses, because both would otherwise fabricate a handle and assert against it:

* the account is **handle-less** on the nest (``verify.handle`` empty → the
  machine falls back to the typed handle), so the probe would come back as if
  it were real; and
* the identity is **not registered** on the box (an unclaimed or foreign nest),
  where the wizard routes to claim/invite instead of signing in — claiming a
  live box under a probe handle is precisely the damage this module must not do.

Both are recoverable by the caller setting ``FAUNA_LIVE_HANDLE`` explicitly,
which is why the env var survives as an **override** and never as a
precondition.
"""

from __future__ import annotations

import os
import time

# A localpart no deployment would hand out, so "the derivation silently returned
# what we typed" (the handle-less account case) is detectable rather than
# plausible. Valid under the handle charset — 3–63 lowercase alphanumerics and
# hyphens, no edge hyphens (`nest/public-mode.md` § User Registration) — because
# a malformed probe would fail the check at `FormatInvalid` and never reach the
# silent challenge that does the real work.
PROBE_LOCALPART = "fauna-e2e-handle-probe"

_OVERRIDE_VARS = ("FAUNA_LIVE_HANDLE", "FAUNA_LIVE_MAIL_ADDRESS")


def handle_override() -> str:
    """The caller-supplied handle, or ``""``. An override, never a
    precondition — a live test must run without it."""
    for var in _OVERRIDE_VARS:
        value = (os.environ.get(var) or "").strip()
        if value:
            return value
    return ""


def nest_host(nest_url: str) -> str:
    """The ``host[:port]`` a handle on this box qualifies with.

    Mirrors `fauna_core::resolve::qualify_handle`'s node-address half
    (`libs/fauna-core/src/resolve.rs:371`): the handle domain is the nest's own
    host, and a non-default port is part of it.
    """
    rest = nest_url.split("//", 1)[-1]
    return rest.split("/", 1)[0].strip()


def qualify(local_or_full: str, nest_url: str) -> str:
    """``localpart`` → ``localpart@host``; an already-qualified handle passes
    through. The Python twin of `fauna_core::resolve::qualify_handle`."""
    handle = local_or_full.strip()
    if not handle or "@" in handle:
        return handle
    host = nest_host(nest_url)
    return f"{handle}@{host}" if host else handle


def sign_in_handle(nest_url: str) -> str:
    """What to type into ``handle-input``.

    The override when set (so an explicit handle still drives claim-fresh and
    foreign-nest flows), else a probe on the box's own host — the nest replaces
    it with the registered handle at ``AlreadyOnNest``.
    """
    override = handle_override()
    return qualify(override, nest_url) if override else qualify(PROBE_LOCALPART, nest_url)


def derive_handle(app, nest_url: str, *, timeout: float = 90.0) -> tuple[str, str]:
    """``(handle, localpart)`` for the signed-in account — nest-authoritative.

    Call once the app has reached the logged-in shell. Polls, because the handle
    arrives on the account fetch that follows auth rather than with the shell
    itself (`app.rs` ``AccountLoaded``); the budget is a generous ceiling in the
    convention-14 sense, not a settle-sleep — a green run pays only the true
    latency.
    """
    override = handle_override()
    if override:
        qualified = qualify(override, nest_url)
        return qualified, qualified.split("@", 1)[0]

    deadline = time.monotonic() + timeout
    seen = ""
    while time.monotonic() < deadline:
        try:
            seen = (app.driver.get_state("session.handle") or "").strip()
        except Exception:  # noqa: BLE001 — a mid-launch read may transiently fail
            seen = ""
        if seen and seen != PROBE_LOCALPART:
            return qualify(seen, nest_url), seen.split("@", 1)[0]
        time.sleep(1.0)

    if seen == PROBE_LOCALPART:
        raise AssertionError(
            f"the nest reports NO handle for this identity: `session.handle` "
            f"came back as the probe localpart {PROBE_LOCALPART!r}, which the "
            "onboarding machine substitutes when the registered handle is empty "
            "(machine.rs `current_handle.unwrap_or(typed_handle)`). This account "
            "is handle-less on the box, so there is nothing to derive — set "
            "FAUNA_LIVE_HANDLE explicitly if you meant to target it."
        )
    raise AssertionError(
        f"could not derive the admin handle from FAUNA_LIVE_SECRET_HEX within "
        f"{timeout:.0f}s: `session.handle` stayed {seen!r}. The app reached the "
        "logged-in shell, so the account fetch that carries the handle either "
        "never landed or the identity is not registered on this box — set "
        "FAUNA_LIVE_HANDLE to override."
    )
