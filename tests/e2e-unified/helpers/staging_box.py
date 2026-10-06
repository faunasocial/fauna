"""Pure planning helpers for the **kept staging box** live test
(``tests/live/test_staging_box_provision.py``).

Everything here is a function of its arguments — no network, no driver — so
the decisions that would be expensive to get wrong against a real, paid, kept
box are pinned by a tier_1 test (``tests/test_staging_box_plan.py``) instead of
being discovered mid-provision:

  * what the run does when it finds provider resources at the box's name
    (:func:`decide_start`, over :func:`server_name_for` and :func:`rrsets_under`);
  * where the box's admin credentials live between runs
    (:func:`load_or_create_state`).
"""
from __future__ import annotations

import json
import os
import secrets
from pathlib import Path

#: Provider-side label the kept box carries. Deliberately NOT the throwaway
#: ``fauna-e2e=1`` label: every live test's teardown deletes any server with
#: that label once it is an hour old (``tests/live/conftest.py``'s orphan sweep).
STAGING_LABEL_KEY = "fauna-staging"
STAGING_LABEL_VALUE = "1"

#: Per-machine home of the kept box's admin credentials (seed, handle, mail
#: password). Outside every repository, like the provider token's own file.
STATE_DIR = "~/.config/fauna/staging-box"

#: :func:`decide_start` verdicts.
PROVISION = "provision"
RESUME = "resume"


class StagingPlanError(ValueError):
    """The run cannot proceed from what it found — raised BEFORE anything is
    provisioned, so the refusal costs nothing."""


def server_name_for(domain: str) -> str:
    """The server name the provisioning orchestrator gives a box for ``domain``
    (it replaces dots with dashes)."""
    return domain.replace(".", "-")


def zone_for(domain: str, zone_names: list[str]) -> str | None:
    """The longest of ``zone_names`` that ``domain`` sits in (or equals)."""
    domain = domain.rstrip(".").lower()
    best = None
    for name in zone_names:
        z = name.rstrip(".").lower()
        if domain == z or domain.endswith("." + z):
            if best is None or len(z) > len(best):
                best = z
    return best


def relative_name(domain: str, zone: str) -> str:
    """``domain`` as a zone-relative RRset name (``@`` for the apex)."""
    domain, zone = domain.rstrip(".").lower(), zone.rstrip(".").lower()
    if domain == zone:
        return "@"
    return domain[: -len(zone) - 1]


def rrsets_under(rrsets: list[dict], rel: str) -> list[dict]:
    """The RRsets that belong to the box named ``rel`` (zone-relative): the
    name itself and everything below it (``mail.<rel>``, ``_dmarc.<rel>``,
    ``<selector>._domainkey.<rel>``). The apex (``@``) is refused — a kept box
    at a zone apex could not be told apart from the rest of the zone."""
    if rel == "@":
        raise StagingPlanError(
            "a staging box at the zone apex is refused: its RRsets cannot be "
            "told apart from the rest of the zone"
        )
    suffix = "." + rel
    return [r for r in rrsets if r.get("name") == rel or r.get("name", "").endswith(suffix)]


def decide_start(*, state: dict, servers: list[dict], rrsets: list[dict]) -> str:
    """What a run does given what already exists at the box's name.

    * nothing exists                      → :data:`PROVISION`
    * the server this machine's state file records as provisioned-and-claimed
      exists                              → :data:`RESUME` (sign in, finish the
      remaining steps — every one of them is idempotent)
    * anything else                       → refuse. A server this state file did
      not record is either someone else's box or one whose provisioning died
      before the claim; a run that provisioned over it would mint a fresh
      identity for a box that already has one. Stray RRsets with no server
      would be silently adopted as the new box's records.

    A START never deletes. A run that FAILS before the claim removes what it
    just created (`cleanup_plan`); anything found here that it does not
    recognise is left for a person.
    """
    if not servers and not rrsets:
        return PROVISION
    recorded = state.get("server_id")
    if recorded is not None and any(s.get("id") == recorded for s in servers):
        return RESUME
    raise StagingPlanError(
        f"{state.get('domain')} already exists, in whole or in part, and this "
        "machine's state file does not record it as a box this test provisioned "
        f"and claimed (recorded server id: {recorded!r}). Found servers "
        f"{[(s.get('id'), s.get('name')) for s in servers]} and RRsets "
        f"{[(r.get('name'), r.get('type')) for r in rrsets]}. This test never "
        "replaces or deletes a box: remove them deliberately, then rerun."
    )


def cleanup_plan(*, state: dict, servers: list[dict], rrsets: list[dict]) -> dict:
    """What a FAILED run removes: the server and records at the box's name —
    but only while the state file does not record the box as built and
    claimed.

    * not recorded (`server_id` absent): the run failed before the claim. The
      box can never be claimed — its claim code lived in the app instance that
      just ended — so it is dead weight that would block every rerun. Remove
      the server(s) and RRsets at the name. `decide_start` proved, before this
      run created anything, that nothing was there, so everything found is
      this run's own.
    * recorded: the box is claimed and a rerun resumes it. Remove nothing.

    This is cleanup, never a route to green: it is only ever called from a
    failing run's `except` branch, which re-raises.
    """
    if state.get("server_id") is not None:
        return {"servers": [], "rrsets": []}
    return {"servers": list(servers), "rrsets": list(rrsets)}


def state_path(domain: str) -> Path:
    return Path(os.path.expanduser(STATE_DIR)) / f"{domain}.json"


def load_or_create_state(domain: str, *, localpart: str = "admin") -> dict:
    """The kept box's admin credentials, created on first use and reused after.

    Written BEFORE anything is provisioned and with mode 0600: the seed is the
    only key to a box the run then claims, so a crash between the claim and a
    later write would strand a paid, claimed, unreachable-as-admin server. A
    rerun reuses the same identity.
    """
    path = state_path(domain)
    if path.exists():
        state = json.loads(path.read_text(encoding="utf-8"))
        if state.get("domain") != domain or not state.get("secret_hex"):
            raise StagingPlanError(
                f"{path} does not describe {domain!r} (or holds no seed); move "
                "it aside rather than letting a run overwrite it"
            )
        return state
    state = {
        "domain": domain,
        "handle": f"{localpart}@{domain}",
        "secret_hex": secrets.token_bytes(32).hex(),
        "mail_password": secrets.token_urlsafe(24),
    }
    save_state(state)
    return state


def save_state(state: dict) -> Path:
    path = state_path(state["domain"])
    path.parent.mkdir(parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(state, f, indent=2, sort_keys=True)
        f.write("\n")
    os.chmod(path, 0o600)
    return path
