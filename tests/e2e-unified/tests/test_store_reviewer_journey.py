"""tier_4 (live-remote, OPT-IN, NON-DESTRUCTIVE): a store reviewer's session,
walked the way a Google Play, App Store or Microsoft Store reviewer is likely
to walk it — on a staging box, never on the acceptance nest.

Why this exists: every store reviews a build by signing in to the
organisation's acceptance nest as that store's own reviewer account
(`org-nests.md` § fauna.build → *Accounts*; `installers/store-review.md`
§ The tracks, *What a review needs from the organisation*). The provisioning
drive proves the accounts are admitted and seeded; nothing proved that a
reviewer, once in, can do what a reviewer checks. A reviewer of an app with
user content checks first that the app is not empty and works, then that the
safety promises of the certification notes hold (`terms-and-content-
guidelines.md` § Certification notes wording: reporting on every post, message
and profile, blocking any account, the terms in Settings), and that an account
can be deleted from inside the app. The walk itself — the reviewer script,
step by step, and what it mutates — is
``helpers/store_reviewer_walk.py``, shared with the tier_3 local twin
(``tests/test_store_reviewer_journey_local.py``, a standalone nest seeded the
way the provisioning drive seeds the boxes).

The acceptance nest is refused as a target by the harness
(`nest_mode.refuse_acceptance_nest`; `testing.md` § The shared-box rule): a dev
machine signs in there once per re-creation, and only the re-creation does.
So this runs on the two staging boxes, `dev.fauna.build` and
`test.fauna.build` (`nest_mode.DISPOSABLE_BOX_HOSTS`), once the same org
accounts are admitted and seeded there: their identity files sit where the
provisioning drive keeps them, ``~/.config/fauna/staging-box/<box>/accounts/
<handle>.json`` (``helpers.staging_box.account_path``) — the three reviewers
and the tester. Missing any of them, the test skips naming the files.

Blast-radius argument (testing.md § The shared-box rule, non-destructive
carve-out):
  - **What it mutates, and what teardown removes:** the walk module's
    docstring — run-token content only, all of it deleted in teardown, every
    report withdrawn, the block and the account report aimed at another
    reviewer, never the tester.
  - **Device slots.** Every leg signs in on a reset app instance, first
    removes any device a dead earlier leg left, and ends with Settings
    sign-out (``helpers.org_account_session``), so the accounts' 3-device free
    tier is left as found.
  - **Why invisible to a human on the box:** nothing of anyone else's is
    touched — no admin page, no setting, no other account's data; the lock on
    Sessions, which no admin can lift (`org-nests.md` § fauna.build →
    *Accounts*), is never pressed, and account deletion is never armed.
  - **Never a factory reset**, and none must be added.

**The app is the release's** (``org-nests.md`` § fauna.build → *Disposable*,
rule (1)): the ``release_app`` marker launches a client built from the private
commit the box serves (its health ``commit``, paired through the
``Sync-Point:`` trailer when that is a public build — ``helpers/release_app.py``),
never the tip's; a commit that cannot be resolved stops the run before any
sign-in.

Run it: ``just e2e-store-reviewer <box> [app] [args]`` — for example
``just e2e-store-reviewer test.fauna.build`` or ``just e2e-store-reviewer
dev.fauna.build android``; ``-k google`` picks one store's reviewer.

Test taxonomy: tier_4 (live-remote — the deployed image under real
supervision, reached by a real app over the internet).
"""

from __future__ import annotations

from urllib.parse import urlsplit

import pytest

from helpers import live_box_door, nest_mode
from helpers.staging_box import REVIEWER_ACCOUNTS, account_path, load_or_create_account
from helpers.store_reviewer_walk import ACCOUNTS, Nest, run_reviewer_journey

URL = live_box_door.live_box_url()
BOX = (urlsplit(URL).hostname or "").lower().rstrip(".") if URL else ""
MISSING = [str(account_path(BOX, lp)) for lp in ACCOUNTS
           if BOX and not account_path(BOX, lp).exists()]


def _skip_reason() -> str:
    if not URL:
        return ("store reviewer journey: name the box (`--nest live:https://<box>`, or "
                "FAUNA_LIVE_NEST_URL) — dev.fauna.build or test.fauna.build; opt-in, "
                "non-destructive — see the module docstring")
    if BOX not in nest_mode.DISPOSABLE_BOX_HOSTS:
        return (f"store reviewer journey: {BOX!r} is not a staging box "
                f"({', '.join(sorted(nest_mode.DISPOSABLE_BOX_HOSTS))}); the reviewer "
                "journey signs in as org-minted accounts and runs only there")
    if MISSING:
        return ("store reviewer journey: the org accounts are not minted for "
                f"{BOX} on this machine — missing {', '.join(MISSING)}. The staging-box "
                "provisioning drive mints, admits and seeds them on its next run there "
                "(org-nests.md § fauna.build → Accounts)")
    return ""


pytestmark = [
    pytest.mark.tier_4,
    pytest.mark.live_nest,
    pytest.mark.live_box,
    # The tester's run-token message reaches the reviewer over real MLS, and
    # the native apps start the receive loop under e2e only behind this
    # marker's launch gate (the local twin's docstring, *The conversations
    # are real*).
    pytest.mark.real_conversations,
    # The release's own app, never the tip's: a reviewer meets the release's
    # app against the release's nest, so that pair is the one walked
    # (org-nests.md § fauna.build → Disposable, rule (1)) — built from the
    # commit the box's /api/v1/health serves (`conftest._release_app_override`).
    pytest.mark.release_app(box=BOX if not _skip_reason() else None),
    pytest.mark.skipif(bool(_skip_reason()), reason=_skip_reason() or "runnable"),
]


@pytest.mark.parametrize("reviewer", REVIEWER_ACCOUNTS)
def test_a_store_reviewer_can_do_what_a_reviewer_checks(app, reviewer):
    accounts = {lp: load_or_create_account(BOX, lp) for lp in ACCOUNTS}
    # A staging box runs `mail.auto_enable_for_new_users` at its default, on.
    nest = Nest(where=BOX, status_domain=BOX, report_name=BOX, mail_state="on")
    run_reviewer_journey(app, accounts, reviewer, nest)
