"""tier_3 E2E: the anonymous discovery budget does not throttle a harness run.

`fauna.nest.info` rides the throttled anonymous discovery surface
(`bins/fauna-nest/src/anonymous_rate_limit.rs`; gate at `routes.rs`, kind set at
`pre_identity_allowlist::is_throttled_anonymous_kind`), whose shipped budget is
**60 events / 60 s per bucket** — the directory-harvesting bound
(`federation.md` § Security). That number is deliberately not raised: the module
says so in its own words, and a release-flavour unit test pins it.

The harness cannot live under it. Every e2e client is `127.0.0.1` against one
**session-scoped** nest — deliberate, `e2e-conventions.md` point 10 ("the answer
is never to isolate it") — so both of the gate's bucket classes collapse to ONE
bucket for a whole run: anonymous traffic keys on the peer IP, and the suite's
session-scoped actors put many tests behind a single `actor_id`. A per-source
budget over a single shared source is a budget **per run**, not per test. So a
harness build raises it via a `test-hooks` compile arm, exactly as the
registration budget already does.

**Why this test exists on top of the unit pins.** The unit tests assert the
*constant* per build flavour. They cannot see the wiring — that `AppState`
actually builds its limiter from `default_config()`, that the gate actually
consults it for this kind, or that `build_node()` actually compiles the nest
with `test-hooks`. This is the end-to-end arm: >60 real `fauna.nest.info` calls
over the real anonymous WS surface against the real binary the suite runs, all
of which must be answered.

Reddens against the pre-fix tree: at the shipped 60 the 61st call returns
`fauna.protocol.rate_limited` and this fails on the exact error the batch-13
web failures carried in their browser console — an unrelated-looking
`feedReady=False` whose real cause was this budget.

Not a timing test (convention 14): the window is a *count* over a sliding
minute, and the assertion is on how many calls are answered, not on how long
anything took. A slow machine makes the burst span more wall-clock, which can
only make the sliding window MORE forgiving — never less — so the test cannot
red from load.
"""

import pytest

from tests.api import ws_api
from clients.ws_rpc_anon_client import RpcCallError

# `standalone_only` because the raised budget this asserts is a `test-hooks`
# COMPILE-TIME constant, and a release artifact cannot carry one. nest says so
# itself, in `anonymous_rate_limit.rs`'s "Scope, stated honestly" paragraph
# above `DISCOVERY_MAX_EVENTS`: *"This reaches tier_3, which builds
# `--features test-hooks` (`conftest.py`'s `build_node`). **tier_4 does not** —
# it runs the real Docker image, which carries no test hooks by design, so a
# tier_4 run still meets the shipped 60. That is the correct boundary, not a
# gap to close later."* So a docker-mode red here accuses the image of a defect
# it does not have — the artifact is behaving exactly as convention 15
# (`e2e-conventions.md` point 15) requires.
#
# ⚠ **The marker is legitimate here for one reason only: the budget is this
# test's SUBJECT.** `testing.md` § Default app and nest mode (*"a shipped limit
# is a test constraint, not a route into that classification"*, ratified
# 2026-08-30) leaves `standalone_only` available only to a test that genuinely
# needs more than a shipped budget inside one window — which is this file's
# whole thesis — while a journey test that merely *spends* the budget must be
# made to fit it instead (`test_family.py`: 30 submits cut to 6, no
# reclassification). Do not copy this marker to a test of the second kind; there
# it would blank a cell that is a coverage hole.
#
# No inferred class can catch this: class (6) (`RULE_TEST_HOOKS`) matches the
# test-hooks **HTTP surface** by route prefix and this test reaches no route —
# convention 15's boundary shows up in the artifact as absent routes AND as
# different compile-time constants, and only the first is derivable. The marker
# is `nest_surface._verdicts`' first rule, so this still lands as a tallied
# `DECLARED_ABSENCE`, not a silent skip. 
pytestmark = [pytest.mark.tier_3, pytest.mark.standalone_only]

# Comfortably past the shipped 60-per-60s bound, so a release-flavour budget
# trips well before the last call. Not so large that the test is slow: each
# call is one short-lived anonymous WS connection to loopback.
CALLS = 75


def test_a_harness_nest_answers_more_discovery_calls_than_the_shipped_budget(
    nest_instance,
):
    """>60 `fauna.nest.info` calls in one burst, all answered.

    The failure message names the mechanism rather than the symptom, because
    the symptom is what cost three batch-13 files a day of misdiagnosis: the
    refusal lands inside whatever test happens to be running and reads as that
    test's own product bug.
    """
    port = nest_instance["port"]

    answered = 0
    for i in range(CALLS):
        try:
            reply = ws_api.nest_info(port)
        except RpcCallError as e:
            code = getattr(e, "code", "") or str(e)
            if "rate_limited" in code:
                pytest.fail(
                    f"the discovery budget throttled call {i + 1} of {CALLS} "
                    f"({code}). This nest was built without the `test-hooks` "
                    "harness arm on `anonymous_rate_limit::default_config`, so "
                    "the whole run shares one 60-per-minute bucket. Every "
                    "later test on this session-scoped nest now fails on "
                    "whatever it happened to be doing when the window closed "
                    "— which is exactly how this was mistaken for three "
                    "unrelated web product bugs."
                )
            raise
        assert reply, f"nest.info call {i + 1} returned an empty reply"
        answered += 1

    assert answered == CALLS
