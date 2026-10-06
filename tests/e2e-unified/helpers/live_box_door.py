"""The live box door: on `--nest live`, failing to REACH the box is environment.

`feature-catalog.md` § Cell semantics: "`skip_environment` outcomes are never
recorded. A missing credential or an unreachable port says nothing about the
feature; the previous ledger line stands." A live run does not own its nest — it
resolves a box somebody else deployed (`testing.md` § Default app and nest mode,
*Live mode*) — so when that box is down, every test behind the session
provisioning fixtures fails in setup with a transport error that is a fact about
the box, not about any feature. Raised raw, pytest reports each of them as a
setup `error` and the ledger records it: one `--nest live` run launched minutes
after a example.com outage on 2026-09-14 wrote 185 such `error`s into tui's ledger,
turning working features ❌ on the public matrix.

`reaching_the_live_box(what)` wraps the harness's first contacts with the box
(the provider's reachability probe, the shared account provisioning) and turns
exactly that class of failure into a declared `skip_environment`. It is
deliberately narrow — `is_unreachable` admits only failures where the box never
answered:

* **In:** a refused / reset connection, a DNS failure, a TLS failure, and a
  gateway status (502 / 503 / 504) at the WebSocket upgrade — the reverse proxy
  saying the nest behind it is not there.
* **Out:** anything the box ANSWERED — a typed `RpcCallError`, any other upgrade
  status (a 500 is the nest failing, a 403 is it refusing). Also out: a link that
  died with a request in flight (`WsLinkDied` — ruled a product signal) and a
  timeout (a reply that never came cannot be told apart from a hang, and a hang
  is a product bug). Those stay recorded `error`s, as `error` is a legal outcome
  (§ The ledger).

Off live the door is transparent: a standalone or docker nest is the harness's
own, so a refused port there is a harness or product failure and stays an error.
"""
from __future__ import annotations

import socket
import ssl
from contextlib import contextmanager

#: Upgrade statuses that mean "the proxy in front of the nest has no nest behind
#: it" rather than "the nest answered".
GATEWAY_STATUSES = frozenset({502, 503, 504})


def _unreachable_here(exc: BaseException) -> bool:
    import websocket  # from `websocket-client`

    if isinstance(exc, (ConnectionError, socket.gaierror, ssl.SSLError)):
        return True
    if isinstance(exc, websocket.WebSocketBadStatusException):
        return getattr(exc, "status_code", None) in GATEWAY_STATUSES
    return False


def is_unreachable(exc: BaseException) -> bool:
    """Whether `exc` — or what it explicitly wraps (`raise … from`) — means the box
    never answered. `__context__` is not followed: an error raised while handling
    a transport failure is a different error."""
    seen: set[int] = set()
    cur: BaseException | None = exc
    while cur is not None and id(cur) not in seen:
        seen.add(id(cur))
        if _unreachable_here(cur):
            return True
        cur = cur.__cause__
    return False


#: Typed answers that, to the ADMIN probe only, say the box is not set up for this
#: run — not that a feature is broken. `fauna.auth.not_registered`: the box holds
#: no identity for the admin seed (unclaimed, reset, or the wrong box's seed).
BOX_NOT_READY_CODES = frozenset({"fauna.auth.not_registered"})


def box_not_ready(exc: BaseException) -> str | None:
    """The reason `exc` — or what it explicitly wraps — says the box is not set up
    for this run, else None. Only meaningful for the admin probe: elsewhere the
    same typed answer is the product speaking (`reaching_the_live_box`)."""
    seen: set[int] = set()
    cur: BaseException | None = exc
    while cur is not None and id(cur) not in seen:
        seen.add(id(cur))
        code = getattr(cur, "code", None)
        if code in BOX_NOT_READY_CODES:
            return (
                f"the box answered {code} to the admin seed — it holds no "
                f"registered identity for it (unclaimed, reset, or this is not "
                f"the box that seed belongs to)"
            )
        cur = cur.__cause__
    return None


@contextmanager
def reaching_the_live_box(what: str, *, admin_probe: bool = False):
    """Run a first contact with the box; on live, an unreachable box skips as
    environment (`app_surface.skip_environment`) naming `what` and the failure.

    `admin_probe=True` marks the harness's first signed round trip with the admin
    seed (`_LiveProvider._admin`, `preflight_admin`). Only there is
    `fauna.auth.not_registered` a fact about the BOX rather than a feature — the
    preflight that keeps "the box is not set up" from being recorded as a product
    failure. Every other typed answer, and the same answer anywhere else, stays a
    recorded `error`/`failed`."""
    from helpers import nest_mode as nest_mode_mod

    if not nest_mode_mod.run_mode().is_live:
        yield
        return
    try:
        yield
    except Exception as exc:
        from helpers.app_surface import skip_environment

        if admin_probe:
            not_ready = box_not_ready(exc)
            if not_ready:
                skip_environment(
                    f"the live box is not ready while {what}: {not_ready} — "
                    f"nothing about any feature was observed, so nothing is "
                    f"recorded (feature-catalog.md § Cell semantics)"
                )
        if not is_unreachable(exc):
            raise
        skip_environment(
            f"the live box did not answer while {what}: {exc!r} — nothing about "
            f"any feature was observed, so nothing is recorded "
            f"(feature-catalog.md § Cell semantics)"
        )


def admin_probe_what(url: str, source: str | None) -> str:
    """The `what` an admin probe hands `reaching_the_live_box`: the box AND the
    source of the seed it offered (`multiseat_config.resolve_secret`), so a
    `not_registered` skip says which seed was refused — a seed meant for another
    box reads as exactly that, never as a reset box."""
    return f"probing {url} with the admin seed from {source or 'an unnamed source'}"


def preflight_admin(url: str, secret_hex: str, source: str | None = None) -> None:
    """Prove the box is up, claimed and knows the admin seed — a signed handshake —
    BEFORE a live test asserts anything about a feature. Skips as environment when
    the box is down or not set up; returns when ready (so a failure after it is a
    real product signal about a box that is demonstrably claimed). `source` names
    where the seed came from, for the skip text (`admin_probe_what`)."""
    from common.auth import mint_token_via_handshake
    from nacl.signing import SigningKey

    with reaching_the_live_box(admin_probe_what(url, source), admin_probe=True):
        mint_token_via_handshake(url, SigningKey(bytes.fromhex(secret_hex.strip())))


# ── Which box a live MODULE drives, and with which seed ─────────────────────
#
# A `live_box` module resolves both at import, for its `skipif` gate. The seed
# is resolved per box (`multiseat_config.resolve_secret`), never read from
# `FAUNA_LIVE_SECRET_HEX` alone, so a staging box this fleet provisioned runs
# with nothing exported (`testing.md` § Default app and nest mode, *Live mode*).
# That makes the seed AMBIENT, so it can never be the opt-in of a destructive
# test: a factory-resetting module gates on the run's disposable-box
# declaration instead (`testing.md` § The shared-box rule → *The disposable-box
# declaration*). The mailbox a destructive mail module re-claims and enables is
# resolved per box the same way (`mailbox`). Pinned by
# `tests/test_live_seed_resolution.py`.

#: Where `admin_seed` looks, for a skip reason to name.
SEED_SOURCES = "FAUNA_LIVE_SECRET_HEX, the box's staging-box file, or ~/.fauna-id"

#: Where `mailbox` looks, for a skip reason to name.
MAILBOX_SOURCES = "FAUNA_LIVE_MAIL_ADDRESS / FAUNA_LIVE_MAIL_PASSWORD, or the box's staging-box file"


def admin_seed(url: str) -> tuple[str, str | None]:
    """``(seed hex, its source)`` for the box at ``url`` — ``("", None)`` when
    ``url`` is empty or nothing provides one (the module's gate then skips)."""
    from helpers.multiseat_config import resolve_secret

    secret, source = resolve_secret(url) if url else (None, None)
    return secret or "", source


def live_box_url() -> str:
    """The box a NON-destructive live module drives: on a `--nest live` run,
    the run's box (`nest_mode.live_url` — the invocation's URL >
    `FAUNA_LIVE_NEST_URL` > the project's live box), so a gate run such as
    `just e2e-cd-gate URL` needs nothing exported; on any other run,
    `FAUNA_LIVE_NEST_URL`, ``""`` when unset (the module's gate then skips —
    outside a live run the export is still what selects the box)."""
    import os

    from helpers import nest_mode

    mode = nest_mode.run_mode()
    if mode.is_live:
        return nest_mode.live_url(mode)
    return os.environ.get("FAUNA_LIVE_NEST_URL", "").rstrip("/")


def mailbox(url: str) -> tuple[str, str]:
    """``(address, password)`` of the mailbox a destructive live mail module
    re-claims and enables on the box at ``url`` — resolved per box
    (`multiseat_config.resolve_mailbox`: env > the box's staging-box file), each
    ``""`` when ``url`` is empty or nothing provides it (the gate then skips)."""
    from helpers.multiseat_config import resolve_mailbox

    address, password = resolve_mailbox(url) if url else (None, None)
    return address or "", password or ""


def destructive_live_box() -> tuple[str, str | None]:
    """``(url, skip reason)`` for a DESTRUCTIVE live module — one that
    factory-resets the box or rewrites its global admin state with no restore.

    The url is the run's declared-disposable box (`nest_mode.live_url`, the
    precedence the declaration itself was validated against), so the box a
    destructive test resets is always the one the invocation named — never a
    `FAUNA_LIVE_NEST_URL` some earlier shell exported beside a declaration for a
    different box. The reason is None only on a run that declared its box
    disposable (`--nest live:URL --live-box disposable`, which `declare_box`
    accepts only for a staging box); every other run skips, whatever seed the
    machine holds."""
    from helpers import nest_mode

    mode = nest_mode.run_mode()
    if mode.disposable_box:
        return nest_mode.live_url(mode), None
    return "", (
        "destructive live test (it factory-resets the box or rewrites its global "
        "admin state): it runs only on a run that declares its box disposable — "
        "`--nest live:https://dev.example.com --live-box disposable` "
        "(testing.md § The shared-box rule → The disposable-box declaration). "
        "The admin seed is resolved per box and is never the opt-in."
    )


def marks_live_box(tree) -> bool:
    """Whether a parsed test module's top-level ``pytestmark`` carries the
    ``live_box`` marker — how the tier_1 live-module ratchets
    (`test_live_seed_resolution.py`, `test_live_handle_derivation.py`) tell a
    live module from a pin that merely names its env vars."""
    import ast

    for node in tree.body:
        if isinstance(node, ast.Assign) and any(
            getattr(t, "id", None) == "pytestmark" for t in node.targets
        ):
            return any(
                isinstance(n, ast.Attribute) and n.attr == "live_box"
                for n in ast.walk(node.value)
            )
    return False
