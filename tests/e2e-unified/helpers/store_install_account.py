"""The account a store-install witness signs in with, and the nest it is on.

Shared by the store-install witnesses — ``tests/artifact/
test_android_store_install.py`` (the app as Google Play delivered it) and its
iOS twin — so both settle the same question the same way. The ruling and its
reasons: ``docs/goal/architecture/feature-catalog.md`` § Implementation status
today, the bullet on how the Android store install is driven, and
``org-nests.md`` § fauna.build → *Accounts*, the witness-host tester.

**The nest: the staging box on the UAT channel, ``test.fauna.build``.** A store
build pins no nest and takes no seed: the release build reaches any nest a
handle names, over TLS only (its network security config refuses cleartext),
so the harness's own plain-HTTP standalone nest is out of reach and the nest
must be a deployed box the device can reach by name. The acceptance nest
``fauna.build`` is the one a version's store build is accepted against, but it
is humans-only — the harness refuses it as a target
(``nest_mode.refuse_acceptance_nest``; ``testing.md`` § The shared-box rule).
``test.fauna.build`` runs the same ``:test`` image (``org-nests.md`` § The three
nests) and is the shared-box rule's automated side, so a version's store build
meets the release it was accepted against.

**The account: this machine's own tester on that box.** The release build's
onboarding registers a fresh identity only with an invite code
(``OnboardingMachine::redeem_invite``), and the organisation's nests are
invite-only (``org-nests.md`` § fauna.build → *Accounts*), so a fresh identity
cannot reach the feed with no seed. The witness signs in exactly as a store
reviewer does — import the identity, type the handle, continue — but as the
witness host's own tester (``<platform>-tester``, `staging_box.
witness_tester_localpart`): an identity minted and kept on the machine that
runs the witness (``just org-tester-mint``), admitted on the box by the
provisioning drive from the tracked registry of public keys. The secret never
leaves this machine; the drive's own identity files (the reviewers') have one
copy, on the dev host, and are not carried anywhere. The witness's subject is
the store install and a sign-in through the app's own UI, not the reviewer
handle, which stays the review notes' alone.

**The reviewer's identity stands in on the dev host alone.** Where this
machine's tester is not minted and registered but the store's reviewer
identity is present — the dev host, where the drive keeps it — the witness
signs in as that reviewer, the account the drive admits and seeds there; the
skip names both routes otherwise. Read only — never minted here: an identity
made by the witness would be admitted nowhere.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

from helpers.app_surface import skip_environment
from helpers.staging_box import (
    account_path,
    actor_hex_for,
    witness_tester_localpart,
    witness_testers_for,
)

#: The staging box on the UAT channel — `nest_mode.DISPOSABLE_BOX_HOSTS`.
STORE_BOX = "test.fauna.build"

#: Each store's own reviewer account on that box (`staging_box.REVIEWER_ACCOUNTS`),
#: the stand-in where this machine's tester is absent.
PLAY_ACCOUNT = "google-reviewer"
APP_STORE_ACCOUNT = "apple-reviewer"


@dataclass(frozen=True)
class StoreAccount:
    handle: str
    secret_hex: str

    @property
    def localpart(self) -> str:
        return self.handle.split("@", 1)[0]

    def __repr__(self) -> str:  # the secret never reaches a failure message
        return f"StoreAccount(handle={self.handle!r})"


def _read_identity(path: Path, localpart: str, box: str) -> StoreAccount:
    account = json.loads(path.read_text(encoding="utf-8"))
    secret = str(account.get("secret_hex") or "")
    if (account.get("domain"), account.get("localpart")) != (box, localpart) or len(secret) != 64:
        skip_environment(f"{path} does not describe {localpart} on {box} with a 64-hex seed")
    return StoreAccount(handle=f"{localpart}@{box}", secret_hex=secret)


def witness_account(reviewer: str, box: str = STORE_BOX) -> StoreAccount:
    """The identity this witness signs in with on ``box``: this machine's own
    tester when it is minted here and registered for ``box`` (the tracked
    registry's entry must name the kept identity's key, or it is admitted
    nowhere); else ``reviewer``'s identity where the drive keeps it; else an
    environment skip naming both routes."""
    tester = witness_tester_localpart()
    tester_path = account_path(box, tester)
    registered = witness_testers_for(box).get(tester)
    if tester_path.is_file() and registered:
        account = _read_identity(tester_path, tester, box)
        if actor_hex_for(account.secret_hex) != registered["actor_hex"]:
            skip_environment(
                f"{tester_path} is not the identity registered as {tester}@{box} in "
                f"tests/e2e-unified/org-testers.toml: re-run `just org-tester-mint {box}` "
                "on this machine and land the registry, so the drive admits the key kept here"
            )
        return account
    reviewer_path = account_path(box, reviewer)
    if reviewer_path.is_file():
        return _read_identity(reviewer_path, reviewer, box)
    skip_environment(
        f"no admitted identity for the store-install witness on {box}: this machine's "
        f"tester {tester}@{box} is {'minted but not registered' if tester_path.is_file() else 'not minted'} "
        f"(`just org-tester-mint {box}` here, then land tests/e2e-unified/org-testers.toml — "
        "the provisioning drive admits it on its next run; org-nests.md § fauna.build → "
        f"Accounts, the witness-host tester), and {reviewer_path} is absent (the drive "
        "keeps it on the dev host alone)"
    )
    raise AssertionError("unreachable")  # pragma: no cover — skip_environment raises
