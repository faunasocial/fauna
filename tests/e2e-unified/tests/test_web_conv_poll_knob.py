"""Pins for the web conversations-rail poll-cadence knob.

The knob's whole job is to make ``test_conv_rail_push_wakes_web`` **able to
fail**: with the backstop ticker muted, the push arm is the rail's only trigger.
Every way the knob can quietly stop working turns that test GREEN for the wrong
reason — the worst outcome available here, and the exact shape row 278 was
filed for (a knob handed to a build that resolved it to nothing, inert for four
days with nobody reporting it).

So the knob's wiring is pinned at tier_1, in every default loop, rather than
resting on the tier_3 test that consumes it:

- the SPA exports ``setConvPollSecs`` and the automation surface installs it
  under the exact ``window.__fauna_*`` name the driver calls — a cross-language
  contract (TypeScript ↔ Python) that no compiler checks, and whose failure mode
  is a ``TypeError`` deep inside a 3-app tier_3 run, or worse, silence;
- the ticker's sleep stays **re-armable**, and the setter re-arms it. This is
  the load-bearing half: a plain ``setTimeout`` in that loop leaves a wake
  pending on the OLD cadence, so muting no longer settles when the next ticker
  sweep lands, and the push-arm attribution becomes a wall-clock race
  (``testing.md`` § conventions, point 14). It reads like a harmless
  simplification, which is why it needs a test and not a comment;
- ``setConvPollSecs`` keeps exactly one importer, ``$lib/e2e-automation``, so it
  is tree-shaken out of production bundles with the rest of the automation
  surface (``testing.md`` § conventions, point 15).
"""

import re
from pathlib import Path

import pytest

from common import get_repo_root

pytestmark = [pytest.mark.tier_1]

_ROOT = Path(get_repo_root())
CONVERSATIONS_TS = _ROOT / "apps/fauna-web/src/lib/conversations.ts"
AUTOMATION_TS = _ROOT / "apps/fauna-web/src/lib/e2e-automation.ts"
WEB_DRIVER = _ROOT / "tests/e2e-unified/drivers/web.py"

# The names both sides must spell identically. Two knobs, one per arm of the web
# receive rail: mute the ticker to isolate the push arm, suppress the push arm to
# isolate the drain. Each is the sole reason its consuming test can fail.
HOOK = "__fauna_setConvPollSecs"
PUSH_HOOK = "__fauna_setConvPushSuppressed"


@pytest.mark.parametrize(
    "setter",
    ["setConvPollSecs", "setConvPushSuppressed"],
)
def test_spa_exports_the_setter(setter):
    src = CONVERSATIONS_TS.read_text()
    assert f"export function {setter}(" in src, (
        f"apps/fauna-web/src/lib/conversations.ts no longer exports {setter} — "
        "the web twins of native's FAUNA_CONV_POLL_SECS / "
        "FAUNA_E2E_SUPPRESS_CONV_PUSH. Without both, the two arms of the web "
        "receive rail can only be proven jointly, and either one can die unnoticed"
    )


@pytest.mark.parametrize(
    "hook,setter",
    [(HOOK, "setConvPollSecs"), (PUSH_HOOK, "setConvPushSuppressed")],
)
def test_automation_surface_installs_the_hook_under_the_driver_s_name(hook, setter):
    automation = AUTOMATION_TS.read_text()
    driver = WEB_DRIVER.read_text()
    assert hook in automation, (
        f"the automation surface no longer installs window.{hook}; a test "
        "calling it gets 'not a function' rather than the behaviour it asked for"
    )
    assert setter in automation, (
        f"e2e-automation.ts installs {hook} but no longer imports {setter}"
    )
    assert hook in driver, (
        f"drivers/web.py no longer calls window.{hook}. This pair is a "
        "cross-language contract no compiler checks: rename one side and the "
        "knob is inert, exactly the failure class already recorded"
    )


def test_push_suppression_covers_the_reconnect_arm_too():
    """Both arms, because native's one env var switches off both.

    ``FAUNA_E2E_SUPPRESS_CONV_PUSH`` makes ``conv_push_source`` return ``None``,
    and ``subscribe_reconnects()`` lives inside that same push source — so a
    suppressed native session has no reconnect sweep either. If web suppressed
    only the push handler, a socket flap would still sweep both rails and
    ``test_fauna_mls_web_receives_from_linux_sender`` would silently stop being a
    drain-only proof, which is precisely the state this pair was built to end.
    """
    src = _strip_comments(CONVERSATIONS_TS.read_text())
    push_arm = _body_of(src, "function subscribeReceivePushes(")
    reconnect_arm = _body_of(src, "function subscribeReconnectSweep(")
    assert "convPushSuppressed" in push_arm, (
        "the conversations push arm no longer honours convPushSuppressed — "
        "suppression would be inert and the drain proof would silently become a "
        "joint proof again"
    )
    assert "convPushSuppressed" in reconnect_arm, (
        "the reconnect arm no longer honours convPushSuppressed. Native's single "
        "env var kills BOTH arms (subscribe_reconnects lives inside the push "
        "source); leaving web's reconnect sweep live makes a socket flap deliver "
        "by poll during a supposedly push-free window"
    )


def test_the_ticker_sleep_stays_re_armable():
    """The setter must cancel and re-arm the sleep already in flight.

    Pinned because the alternative — a bare ``setTimeout(r, pollIntervalMs())``
    in the ticker — is a *smaller-looking* piece of code that silently reverts
    the guarantee ``test_conv_rail_push_wakes_web`` rests on: that once the mute
    returns, the next ticker sweep is a full new interval away.
    """
    src = _strip_comments(CONVERSATIONS_TS.read_text())
    setter = _body_of(src, "export function setConvPollSecs(")
    assert "clearTimeout(pollWake.timer)" in setter, (
        "setConvPollSecs no longer cancels the sleep already in flight, so a "
        "mute leaves a wake pending on the PREVIOUS cadence. The push-arm test "
        "then races that wake instead of excluding it (convention 14)"
    )
    assert "setTimeout(" in setter, (
        "setConvPollSecs cancels the pending sleep but never re-arms it — the "
        "receive rail's ticker would stop for good instead of slowing down"
    )
    loop = _body_of(src, "export async function startReceivePoll(")
    assert "pollWake = {" in loop, (
        "the receive ticker's sleep no longer publishes itself as `pollWake`, "
        "so setConvPollSecs has nothing to re-arm. A bare setTimeout here is "
        "the regression this test exists to catch"
    )


@pytest.mark.parametrize(
    "setter",
    ["setConvPollSecs", "setConvPushSuppressed"],
)
def test_the_setter_has_exactly_one_importer(setter):
    """Convention 15: the automation surface must not reach production code."""
    web_src = _ROOT / "apps/fauna-web/src"
    importers = sorted(
        p.relative_to(_ROOT).as_posix()
        for p in list(web_src.rglob("*.ts")) + list(web_src.rglob("*.svelte"))
        if p != CONVERSATIONS_TS and re.search(rf"\b{setter}\b", p.read_text())
    )
    assert importers == ["apps/fauna-web/src/lib/e2e-automation.ts"], (
        f"{setter} is reachable from production code paths "
        f"({importers}) — it must be imported only by $lib/e2e-automation, "
        "which a production `vite build` constant-folds away "
        "(testing.md § conventions, point 15)"
    )


def _strip_comments(src: str) -> str:
    """Drop block + line comments so a doc-comment can never satisfy a matcher.

    The same scar `feed-refresh-contract.test.ts` records: its first cut matched
    raw source, and the handler's own explanatory comment satisfied the check
    while the real call was gone.
    """
    src = re.sub(r"/\*.*?\*/", "", src, flags=re.S)
    return re.sub(r"^\s*//.*$", "", src, flags=re.M)


def _body_of(src: str, signature: str) -> str:
    """The brace-balanced body of the function opening with ``signature``."""
    if signature not in src:
        raise AssertionError(
            f"apps/fauna-web/src/lib/conversations.ts no longer declares "
            f"{signature!r} — the ticker/setter pair this file pins was renamed "
            "or removed; update these pins in the same change"
        )
    start = src.index(signature)
    open_brace = src.index("{", start)
    depth = 0
    for i in range(open_brace, len(src)):
        if src[i] == "{":
            depth += 1
        elif src[i] == "}":
            depth -= 1
            if depth == 0:
                return src[open_brace : i + 1]
    raise AssertionError(f"unbalanced braces after {signature!r}")
