"""The nest axis's honesty machinery — why a test did NOT run in this mode.

The `nest_surface` twin of `app_surface.py`, called for by `testing.md`
§ Default app and nest mode, *Mode mechanics (ratified 2026-08-01)*: mode
eligibility is **classified exclusion, not opt-in** — by default a test runs in
all three modes, and every exclusion is *inferred* and *tallied* rather than
hand-listed, "so 'runs in all modes' stays falsifiable".

`nest_mode.py` owns the mode vocabulary (what the modes are, what a handle can
answer). This module owns the question that vocabulary raises: when a test does
not run in the selected mode, **which of the ratified classes was it, and is
that class shrinking?**

The failure mode both modules exist to prevent is the same one convention 7
records: `app_capabilities.py` answered "not implemented" for an app it had
never heard of, and every state assertion on tui skipped in silence, reporting
`s` in a summary line that read like success. A hand-maintained table of which
tests run in which mode would reproduce that exactly — so every list below is
a fact about the harness's own fixtures rather than a judgement about any test,
and **every one of them is pinned by a tier_1 test that re-derives it from the
source it describes** (`LOCAL_NEST_FIXTURES` and `NEST_BINARY_FIXTURES` from
conftest's AST, `BRIDGE_SPAWN_FIXTURES` from the bridge-spawn call graph,
`OWN_IMAGE_FIXTURES` from the `tests/platform/docker/` package's). A list that
cannot be re-derived does not belong here.

The three classes, matching `app_surface`'s names on purpose (priority #3 — the
same concepts everywhere):

    mode_unbuilt      temporary debt: this mode COULD run the test, but the
                      harness support is not built yet. Tallied and printed
                      every run; fails under `--strict-nest`.
    declared_absence  structural: this mode cannot run it, ever, and a goal doc
                      says so (there is no virgin live box; convention 15
                      compiles the instrumentation surface out of the image).
                      Always excluded, citation required.
    skip_environment  the box cannot host this mode right now (no docker
                      daemon, image absent). Not a property of the test.

**On the "tally + ratchet" the design record asks for.** The tally is
`gate_hits()`, printed by conftest at the end of every non-standalone run. The
*ratchet* is deliberately NOT a down-only count in a checked-in baseline file,
the way `scripts/check_app_gate_ratchet.py` works for the app axis. The app
ratchet has to be a count because an app-gated skip is only discoverable by
running the test; the things that actually grow the nest axis's excluded set
are the fixture lists below, every one of which is **statically derivable from
the source it describes** — so `test_nest_mode_axis.py` pins each as an
*equality* instead. That is strictly stronger than a down-only count (it catches
an unjustified addition, not merely
a net increase), it needs no baseline file to drift, and it runs in tier_1 on
the default path rather than only in a docker sweep. A count-based ratchet on
top would be redundant machinery that could only ever agree with it.
"""

from __future__ import annotations

import functools
import os
import pathlib
from typing import Iterable, NamedTuple

import pytest

from helpers import kind_reach
from helpers import nest_mode as nest_mode_mod
from helpers.fixture_closure import real_fixture_closure

# ── Run-level state, set once by conftest's pytest_configure ────────────────
# Mirrors `app_surface._STRICT_APP` for the same reason: the helpers are reached
# from fixtures and action code with no `request` in scope.
_STRICT_NEST = False

class GateHit(NamedTuple):
    """One classified exclusion, in hit order.

    `repo_id` and `features` are the **join keys**, and they exist because the
    class tally on its own cannot name a single feature page. A cell may only be
    stamped by a run that collected the feature's whole tagged set
    (feature-catalog.md § Cell semantics), so one gated witness blanks a page's
    cell for that mode outright — and *which class* gated it is the entire
    diagnosis: a fact about the artifact (the cell is honestly blank, forever)
    or harness debt (`mode_unbuilt`, closable).

    Both are collection-time knowledge and neither survives the moment: the nest
    axis deselects in `pytest_collection_modifyitems` *before*
    `_apply_feature_axis` tags anything, so a gated item never acquires the
    ledger's repo-relative id and its slugs are never read. Recorded here or
    lost. They default empty because the runtime declarations (`mode_unbuilt`
    and friends) fire from action code with no item in scope.
    """

    mode: str
    test_id: str
    klass: str
    reason: str
    repo_id: str = ""
    features: tuple = ()
    rule: str = ""
    #: Every rule that applied, not only the deciding one — the audit's
    #: fact-vs-debt verdict needs the whole account, because the ratified order
    #: puts a MIXED rule ahead of a FACT one and the first match therefore
    #: overstates what is closable. Empty for a runtime declaration, which knows
    #: of no rule at all.
    all_rules: tuple = ()
    #: The item's real fixture closure — what the rules matched against.
    #: `classify` computes it anyway; recording it saves the next reader an AST
    #: pass, and the FIXTURE is the unit a fix acts on, so this is what turns a
    #: page list into a work item.
    closure: tuple = ()


#: Every mode-gate this run applied, in hit order. conftest's terminal summary
#: prints the class tally; `gates_payload` is its machine-readable twin.
_GATE_HITS: list[GateHit] = []


def set_strict_nest(enabled: bool) -> None:
    """Called by conftest for `--strict-nest`. Not for test code."""
    global _STRICT_NEST
    _STRICT_NEST = bool(enabled)


def strict_nest_enabled() -> bool:
    return _STRICT_NEST


def gate_hits() -> list[GateHit]:
    """The run's mode-gate hits, for the terminal summary and the ratchet."""
    return list(_GATE_HITS)


def reset_gate_hits() -> None:
    """Test-only: clear the tally between self-test cases."""
    _GATE_HITS.clear()


def record_gate(mode: str, test_id: str, klass: str, reason: str, *,
                repo_id: str = "", features: Iterable[str] = (),
                rule: str = "", all_rules: Iterable[str] = (),
                closure: Iterable[str] = ()) -> None:
    """Tally one exclusion. Called by the collection hook and the helpers.

    The collection hook knows every field and passes them all; a runtime
    declaration knows none of them and passes none.
    """
    _GATE_HITS.append(GateHit(mode, test_id, klass, reason, repo_id,
                              tuple(features), rule, tuple(all_rules),
                              tuple(sorted(closure))))


def gates_payload(*, mode: str, collected: Iterable[str],
                  selection: Iterable[str] = ()) -> dict:
    """The gate tally as data, for the per-page audit that joins it to pages.

    Carries **both halves** of the join on purpose. Gated tests alone cannot
    tell a blanked page from an unwitnessed one: a contract citation that is
    neither collected nor gated is a third thing — an app-axis deselection, a
    `--tier` filter, or a stale citation naming a test that no longer exists —
    and reporting it as a mode exclusion would blame the artifact for a harness
    or catalog fact. `collected` is what the run actually selected, in the same
    repo-relative spelling the contracts cite, so the audit can subtract.

    `selection` is the run's own argv, and it is not bookkeeping: the audit's
    numbers are only meaningful relative to what the run selected. Measured
    2026-08-29, same catalog and same mode — `--app tui` answered "15 of 105
    pages complete, 47 not a mode question", and `--app tui
    --include-independent` answered "18 and 14". The difference is the 126
    `tests/api/` citations an explicit `--app` deselects, which have nothing to
    do with the nest mode. A report that cannot state its own selection invites
    exactly that misreading.
    """
    return {
        "mode": mode,
        "selection": list(selection),
        "gated": [
            {
                "node_id": hit.test_id,
                "repo_id": hit.repo_id,
                "class": hit.klass,
                "rule": hit.rule,
                "all_rules": list(hit.all_rules),
                "closure": list(hit.closure),
                "reason": hit.reason,
                "features": list(hit.features),
            }
            for hit in _GATE_HITS
        ],
        "collected": sorted(set(collected)),
    }


# ── The classes ────────────────────────────────────────────────────────────

MODE_UNBUILT = "mode_unbuilt"
DECLARED_ABSENCE = "declared_absence"
SKIP_ENVIRONMENT = "skip_environment"

# ── The ratified exclusion rules ───────────────────────────────────────────
# The three names above are the SKIP vocabulary, shared with `app_surface`:
# they say what the run should *do* about an exclusion (skip quietly, or fail
# under `--strict-nest`). They are NOT the ratified exclusion classes of
# `testing.md` § Default app and nest mode, and they cannot be — six of the
# seven classes are all `declared_absence`, so a measured docker collection
# reports that one word for all 194 of its exclusions.
#
# These are the classes themselves. Each names one `return` in `classify`, so
# the id is decided where the decision is made rather than recovered afterwards
# by matching the reason prose — which would be a second source of truth for a
# question the classifier already answered unambiguously.
#
# The fact/debt column is the audit's whole point: a cell blanked by a fact
# about the artifact is honestly blank forever (feature-catalog.md § Cell
# semantics — an empty cell is "a claim about our records, nothing more"),
# while one blanked by harness debt is a closable gap.
RULE_MODE_MARKER = "mode_marker"
RULE_NEST_BINARY = "nest_binary"
RULE_LOCAL_NEST_FIXTURE = "local_nest_fixture"
RULE_UNSUPPORTED_OPTION = "unsupported_option"
RULE_OWN_IMAGE = "own_image"
RULE_BRIDGE_SPAWN = "bridge_spawn"
RULE_LOOPBACK_KIND = "loopback_kind"
RULE_PRIVATE_PEER = "private_peer"
RULE_TEST_HOOKS = "test_hooks"
RULE_GLOBAL_ADMIN = "global_admin"
RULE_VENUE_OPTION = "venue_option"
RULE_VENUE_HOST_AFFORDANCE = "venue_host_affordance"
RULE_FEATURE_SET_BINARY = "feature_set_binary"
RULE_PERMANENT_OPTION = "permanent_option"
RULE_IN_PROCESS_TIER = "in_process_tier"
RULE_ABSENT_CAPABILITY = "absent_capability"
RULE_ONE_LIVE_BOX = "one_live_box"
RULE_HARNESS_BOX = "harness_box"

#: Rule id → the one-line label the audit prints. Every id `classify` can return
#: appears here; `test_nest_mode_axis.py` pins that, so a new rule cannot reach a
#: report as a bare identifier.
RULE_LABELS = {
    RULE_MODE_MARKER: "declared @pytest.mark.<mode>_only",
    RULE_NEST_BINARY: "stands up its own locally-compiled nest binary",
    RULE_LOCAL_NEST_FIXTURE: "a conftest fixture spawns a local nest binary",
    RULE_UNSUPPORTED_OPTION: "wants a per-nest start option this mode cannot honour",
    RULE_OWN_IMAGE: "boots its own nest image, so it witnesses that artifact",
    RULE_BRIDGE_SPAWN: "spawns a bridge process on the host (loopback-gated)",
    RULE_LOOPBACK_KIND: "drives a loopback-gated kind itself",
    RULE_PRIVATE_PEER: "hands one nest the private-range address of another",
    RULE_TEST_HOOKS: "drives the test-hooks HTTP surface, absent from a release build",
    RULE_GLOBAL_ADMIN: "mutates global admin state (live only)",
    RULE_VENUE_OPTION: "wants a mail-venue shape this mode cannot stand up",
    RULE_VENUE_HOST_AFFORDANCE: "reads a HOST-spawn affordance off its mail venue",
    RULE_FEATURE_SET_BINARY: (
        "needs a nest COMPILED with test-hooks, which no release artifact is"
    ),
    RULE_PERMANENT_OPTION: (
        "wants a start option this mode can NEVER honour, by ratification"
    ),
    RULE_IN_PROCESS_TIER: "is tier_1 — in-process, no nest for a mode to choose",
    RULE_ABSENT_CAPABILITY: (
        "reads a local-machine capability off the nest handle (live only)"
    ),
    RULE_ONE_LIVE_BOX: "needs two distinct nests; every live nest is one box (live only)",
    RULE_HARNESS_BOX: (
        "asserts a premise only a nest this harness started holds (live only)"
    ),
}

#: The rules whose ratified verdict is **FACT** — the exclusion is a property of
#: the artifact or of the product's own posture, so no harness work will ever
#: close it. The complement is MIXED: closable, at least in part.
#:
#: The grading table itself — the same rules with the sentence justifying each
#: call — lives with the nest-mode audit tool, and that is still its home. This set is the same fact placed where a consumer in
#: THIS tree can reach it, since a test here that imported that tool would make
#: the curated tree fail at import. The two cannot drift: they are pinned equal
#: by `tests/scripts/test_features_mode_audit.py`, which can import both.
FACT_RULES = frozenset({
    RULE_MODE_MARKER,
    RULE_OWN_IMAGE,
    RULE_BRIDGE_SPAWN,
    RULE_LOOPBACK_KIND,
    RULE_PRIVATE_PEER,
    RULE_TEST_HOOKS,
    RULE_GLOBAL_ADMIN,
    RULE_VENUE_HOST_AFFORDANCE,
    RULE_FEATURE_SET_BINARY,
    RULE_PERMANENT_OPTION,
    RULE_IN_PROCESS_TIER,
    RULE_ABSENT_CAPABILITY,
    RULE_ONE_LIVE_BOX,
    RULE_HARNESS_BOX,
})


class Verdict(NamedTuple):
    """Why a test must not run in a mode.

    `klass` is what the run does about it; `rule` is which ratified class
    decided. Both are needed and neither substitutes for the other.
    """

    klass: str
    reason: str
    rule: str

#: Conftest fixtures that spawn a LOCAL `fauna-nest` binary themselves rather
#: than resolving through the mode's provider.
#:
#: **Now a subset of what `NEST_BINARY_FIXTURES` catches generically** — these
#: fixtures request `nest_binary`, so the closure rule already excludes their
#: tests. It is kept as the *named* layer under the general one for two reasons:
#: its AST pin re-derives the set from `_make_nest`'s transitive call graph, so it
#: still catches a dedicated-nest fixture that obtains the binary by some route
#: other than the fixture; and the pin's failure message names the fixture, which
#: is a better diagnosis than a closure match. If the two ever disagree, the
#: closure rule is the one that decides — this list only ever adds.
#:
#: A test depending on one is
#: standalone-only *by construction*: in a docker run it would still get a local
#: binary, so it would report a green "docker" result for a nest the image never
#: served — precisely the silent-fallback failure `nest_mode.NestModeError`
#: refuses at configure time.
#:
#: This is the one hand-written list in the module, and it is a fact about
#: conftest rather than about any test. `test_nest_mode_axis.py` re-derives it
#: from conftest's AST — transitively, because several of these reach
#: `_make_nest` through a private impl helper rather than directly — and fails if
#: the two disagree, so a new dedicated-nest fixture cannot silently widen the
#: standalone-only set.
#:
#: That pin earned itself immediately: this list was first written by reading the
#: direct `_make_nest(` call sites and came out at 13 names, missing 7 that reach
#: it through `_dedicated_mail_nest_impl` / `_bench_mda_impl`. A hand-read table
#: is wrong the day it is written, which is the whole thesis of this module.
#:
#: **Arm 1's conftest half is finished, and what remains is a partition rather
#: than a leftover.** Four fixtures left this set in one edit
#: (`atproto_hosted_nest`, `atproto_localhost_nest`, `web_hosting_nest`,
#: `registration_posture_nest`), and routing them through
#: `_start_dedicated_nest` was the WHOLE change: each asks for `claim_domain` or
#: for nothing, options every provider already honours, so no table below had to
#: move to let them collect in a container. What stays does so for exactly two
#: STRUCTURAL reasons, neither of them "not got to yet":
#:
#: * **It spawns a HOST BRIDGE** — `_dedicated_mail_nest_impl`'s four,
#:   `_bench_mda_impl`'s two, `restartable_mda_nest`. The nest is the easy
#:   half; the MTA/MDA beside it are class (5), and the docker shape of a mail
#:   nest is the image's own s6 bridges enabled through the product's toggle
#:   (`testing.md` § Default app and nest mode, ruling (3)) — arm 6's build, not
#:   a routing edit.
#: * **It asks for `handle_domain_seed` AND spawns a host bridge** —
#:   `unclaimed_mail_nest_ui`. Only the second half is why it is here.
#:
#:   ⚠ This bullet used to read "it asks for `handle_domain_seed` … routing it
#:   would convert a clean collection-time exclusion into a setup-time refusal:
#:   strictly worse", and that reasoning was **wrong** — corrected 2026-09-02,
#:   when seven other `handle_domain_seed` fixtures routed and none of them
#:   produced a refusal. A setup-time refusal is what happens to an option the
#:   CLASSIFIER cannot see; `FIXTURE_START_OPTIONS` is precisely the mechanism
#:   that makes it visible, and the table below is what a routing arm edits in
#:   the same commit. Routed *with* the declaration, an unhonourable option is a
#:   collection-time class (4) naming the option — strictly BETTER than the
#:   `nest_binary` closure it replaces, which is MIXED and reads as closable
#:   work. The old wording described what would happen if you routed and forgot
#:   the table, which is a mistake, not a property of the option.
#:
#:   What keeps this fixture here is the first bullet's reason: it spawns an
#:   unapproved MTA on the host beside its nest, so its docker shape is the
#:   image's own s6 bridges — arm 6's venue seam, not a routing edit.
#:
#: A name here is therefore a claim about the fixture's SHAPE, and the arm that
#: removes it is named. Worth stating because the previous state of this list —
#: thirteen names, some one edit from routable and some not — read as a backlog.
#:
#: ⚠ **This list has never been the whole of "standalone-only", and reading it as
#: such is how four fixtures hid.** Its AST pin roots at `_make_nest`, so a
#: fixture reaching `common.nest.start_nest` DIRECTLY appears in neither this set
#: nor `FIXTURE_START_OPTIONS` (which roots at the two nest-start entry points) —
#: it spawns a local binary that nothing on this axis can see. `handled_nest` was
#: exactly that until 2026-08-30. The general rule still catches such a fixture's
#: tests, because it requests `nest_binary` and the closure rule needs no table;
#: what was invisible is that it *could have routed*. `test_nest_mode_axis.py::
#: test_no_conftest_fixture_spawns_a_nest_behind_both_entry_points` now pins the
#: direct-caller set with a recorded reason per entry.
LOCAL_NEST_FIXTURES = frozenset({
    "bench_mda_encrypted",
    "bench_mda_plaintext",
    "restartable_mda_nest",
    # Sets the DAV toggles before spawning its own MDA, which binds its
    # listener set once at boot — the same own-binaries shape as the fixture
    # above, for the enablement witnesses that must not flip the session nest.
    "dav_toggle_venue",
    # The ONE mail venue that stays, for a NAMED reason rather than "not got to
    # yet" — arm 6 routed the other four through `_start_mail_venue`:
    #   `dedicated_caldav_admin_port_nest` is standalone-only PERMANENTLY, by
    #     ratification — it calls `mda.respawn()`, and s6 owns the rebind in the
    #     image (testing.md § Default app and nest mode, ruling (3)).
    # (`dedicated_caldav_only_nest` was the last to leave: the CalDAV bring-up
    # path it waited on is built — with mail off the image's MDA gates on
    # `/data/caldav-enabled`, so the venue drives `set_caldav_enabled` at setup
    # and converges on that flag and a CalDAV-only listener set.)
    "dedicated_caldav_admin_port_nest",
    "unclaimed_mail_nest_ui",
})

#: Fixture → the per-nest start options it asks a nest for, whether it asks
#: `_make_nest` directly or the mode's provider through `_start_dedicated_nest`.
#: Ruling (3)'s seam, classifier half: subtract the mode provider's
#: `supported_options` from this and a non-empty remainder is a collection-time
#: class (4) — a declared absence naming the option and the mode, rather than a
#: setup-time explosion.
#:
#: **This is what must exist before a fixture may route through a provider**, and
#: it must go on existing AFTER. An unrouted fixture is excluded by its
#: `nest_binary` closure anyway, so the table looks redundant while that is the
#: only state; the moment arm 1 removes the request, this table is the *only*
#: thing standing between an unhonourable option and a `NestModeError` raised
#: from setup — the provider's backstop firing because the classifier that
#: should have spoken earlier could no longer see the fixture. That is why the
#: keys are every nest-starting fixture rather than every local-spawning one:
#: routing changes where a nest comes from, never what the fixture asks it for.
#:
#: ⚠ **"Every nest-starting fixture" means the whole TREE, not the root
#: conftest.** The keys are bare names because that is all `classify` ever gets
#: — `item.fixturenames` carries no path — so a module-local fixture belongs
#: here on exactly the same terms as a conftest one. It reads like a detail and
#: is not: the derivation pin scanned `conftest.py` alone until 2026-09-02, and
#: three fixtures were asking a nest for an option with the classifier unable to
#: see any of them (`consent_nest` `claim_domain`, `cors_seeded_nest`
#: `cors_origins`, `tls_federation_peers` `serve_tls`). Docker honours all three,
#: so nothing was red — which is the point: the gap was invisible precisely
#: where it was harmless, and would have surfaced as a setup-time `NestModeError`
#: the first time a routing arm touched an option this table could not read. It
#: is the same one-directory-below-its-gaze blind spot that hid the duplicated
#: `two_nodes` from the direct-`start_nest` pin, arriving a second time in a
#: different table.
#:
#: The map is exhaustive — the zero-option fixtures are spelled out with an
#: empty set rather than omitted, so an option MOVING is a visible edit to a
#: recorded fact and not an invisible absence. Arm 4 is the case it was written
#: for: `registration_open` left this table for `common.auth.open_registration`,
#: and the diff is four entries shrinking rather than an option quietly ceasing
#: to be mentioned. `handle_domain` was the other half, and it left in three
#: different directions at once — which is the shape of edit this table exists to
#: make legible. Seven fixtures dropped it outright (their own
#: `add_local_domain` registers the primary domain, and registering the primary
#: IS setting the deployment identity, so the option was asking for what the
#: next block already did); three moved to `claim_domain`, which docker honours,
#: so they are no longer class (4) at all; and eight kept it as
#: `handle_domain_seed`, standalone-only forever, for the two reasons the
#: residents' block below states — an `unclaimed=True` nest has no harness claim
#: to carry a domain, and an IP-literal authority is not expressible as a claim
#: at all. Only one of those eight was visible from this table when arm 4 landed:
#: the other seven were spawning their own binaries and appeared here only on
#: 2026-09-02, when they routed. A count in a table is a count of what the table
#: can SEE.
#:
#: Two derivation rules earn their keep, and both were found by measurement:
#:
#: * **A falsy literal is not a request.** `_make_nest`'s defaults are
#:   `False`/`None`, so a fixture spelling one out is asking for the default —
#:   the same rule `_OptionAwareProvider._refuse_unsupported` applies at
#:   runtime, and both must agree or the classifier would gate a fixture the
#:   provider would happily have started.
#: * **A parameter FORWARD belongs to the caller, not the callee.** The pin
#:   resolves a bare `Name` matching an enclosing parameter as "the caller
#:   decides". The case that taught it was `_dedicated_mail_nest_impl` passing
#:   `handle_domain=handle_domain` down to `_make_nest`, which read as a need
#:   made `dedicated_mail_nest` — passing no knobs at all — look like it wanted
#:   two options it did not want. That exact forward is gone (arm 4 deleted the
#:   parameter), and the rule is kept deliberately rather than retired with its
#:   example: it is a property of how impl helpers are written here, not of that
#:   one helper, and `_bench_mda_impl` is one edit away from re-creating it.
FIXTURE_START_OPTIONS = {
    "bridges_pending_nest": frozenset(),
    "bench_mda_encrypted": frozenset(),
    "bench_mda_plaintext": frozenset(),
    "atproto_hosted_nest": frozenset({"claim_domain"}),
    "atproto_localhost_nest": frozenset(),
    "restartable_mda_nest": frozenset(),
    "contribute_nest": frozenset(),
    "crowded_nest": frozenset(),
    "dav_toggle_venue": frozenset(),
    "dedicated_caldav_admin_port_nest": frozenset(),
    "dedicated_no_mail_nest": frozenset(),
    "delete_account_nest": frozenset(),
    # The bluesky-OAuth pair: one option, one difference between them, and that
    # difference IS what they assert (the OAuth client_id derives from the
    # claimed identity domain, so the domained one offers the bridge and the
    # domainless one honestly refuses it).
    "domained_bluesky_nest": frozenset({"claim_domain"}),
    "domainless_bluesky_nest": frozenset(),
    # The consume-side feed journey: a domain (Bluesky OAuth derives its client
    # from it) and the `test-hooks` far-end seam pointing the nest's OAuth
    # client at the harness's `FakeAtprotoFarEnd`.
    "bluesky_feed_nest": frozenset({"claim_domain", "extra_env"}),
    # A private (non-public) deployment for journeys whose nest dials a
    # plaintext loopback URL — the shared nest's pinned `fauna.test` identity
    # makes it public, where the dial policy refuses one.
    "domainless_nest": frozenset(),
    # The same domained/domainless pair for the issuer bridge.
    "domained_issuer_nest": frozenset({"claim_domain"}),
    "domainless_issuer_nest": frozenset(),
    "handled_nest": frozenset(),
    "labeler_grant_nest": frozenset(),
    "nest_instance": frozenset(),
    "provision_target_nest": frozenset({"unclaimed", "cors_origins"}),
    "reclaimable_nest": frozenset(),
    "registration_posture_nest": frozenset({"claim_domain"}),
    "rotatable_nest": frozenset(),
    "rotatable_tls_nest": frozenset({"serve_tls"}),
    "second_nest": frozenset(),
    "self_signed_nest": frozenset({"cors_origins", "serve_tls"}),
    # A nest serving the SPA build, so a browser opens the private share link
    # viewer page at its own `/share/<token>` (`test_share_links_private.py`).
    "share_viewer_nest": frozenset({"static_dir"}),
    "spki_pinned_nest": frozenset({"dial_host", "serve_tls"}),
    "stoppable_nest": frozenset(),
    "third_nest": frozenset(),
    "two_nodes": frozenset(),
    "unclaimed_mail_nest_ui": frozenset({"handle_domain_seed", "unclaimed"}),
    "web_hosting_nest": frozenset({"claim_domain"}),

    # ── MODULE-LOCAL nest fixtures ────────────────────────────────────────────
    # Same table, same meaning, same bare key — a fixture defined in a test
    # module asks a nest for exactly the things a conftest one does, and
    # `classify` cannot tell them apart because `item.fixturenames` carries no
    # path. They are listed here in their own block only so that the arm that
    # routes a family shows as an edit to a recorded fact.
    #
    # The three at the end are why this half exists at all rather than being
    # tidiness: each was already asking a nest for an option, and the table
    # could not see any of them, because the derivation pin read the root
    # `conftest.py` and nothing else. In docker that was harmless (it honours
    # all three options); in live it meant a runtime refusal from the provider's
    # backstop where a collection-time declared absence was available. Left
    # alone, the FIRST option-passing module-local fixture that a routing arm
    # touched would have produced exactly the ❌-against-working-product-code
    # the arm's ordering exists to prevent.
    "admin_auth_nest": frozenset(),
    "archive_nest": frozenset(),
    "box_recovery_nest": frozenset(),
    "bridges_nest": frozenset(),
    "degraded_nest": frozenset({"unclaimed"}),
    "deploy_gate_nest": frozenset(),
    "dr_nest": frozenset(),
    "gated_nest": frozenset(),
    "grant_mint_nest": frozenset(),
    "head_nest": frozenset(),
    "initiator_nest": frozenset(),
    "local_nest": frozenset(),
    "nest": frozenset(),
    "paid_nest": frozenset(),
    "pairing_nest_b": frozenset(),
    "payments_nest": frozenset(),
    "paywall_nest": frozenset(),
    "policy_nest": frozenset(),
    "public_nest": frozenset(),
    "quota_nest": frozenset(),
    "rebuilt_box": frozenset({"extra_env", "unclaimed"}),
    "report_nest": frozenset(),
    "scenario": frozenset(),
    "schema_skew_nest": frozenset({"unclaimed"}),
    "sell_nest": frozenset(),
    "services_nest": frozenset(),
    "signal_nest": frozenset(),
    "skew_nest": frozenset(),
    "snap_nest": frozenset(),
    "subs_consumer_nest": frozenset(),
    "subs_nest": frozenset(),
    "sync_register_nest": frozenset({"claim_domain"}),
    "trust_mint_nest": frozenset(),
    "two_nests": frozenset(),
    "two_report_nests": frozenset(),
    "two_trend_nests": frozenset(),
    "claim_target_nest": frozenset({"unclaimed"}),
    # `extra_env` from its `consent_nest_env` fixture, which a module overrides
    # with a `test-hooks` seam (`test_bridged_conversation_kinds.py`). Read as a
    # forward until 2026-10-06, when the pin learned a fixture's parameters are
    # other fixtures, never a caller's value.
    "consent_nest": frozenset({"claim_domain", "extra_env"}),
    "cors_seeded_nest": frozenset({"cors_origins"}),
    "dav_unclaimed_nest": frozenset({"unclaimed"}),
    "onboarding_nest": frozenset({"unclaimed"}),
    "subdomain_nest": frozenset({"claim_domain"}),
    "tls_federation_peers": frozenset({"serve_tls"}),

    # The APP-JOURNEY family — the first routed fixtures outside `tests/api/`,
    # whose tests drive a real app UI rather than the wire. Every option here is
    # one the docker provider declares, so the entries subtract to empty and the
    # fixtures route rather than becoming class (4).
    #
    # ⚠ Two of them were called `unclaimed_nest` before routing, and the rename
    # is load-bearing rather than cosmetic: this table's key is the BARE fixture
    # name (`classify` reads `item.fixturenames`, which carries no path), so a
    # generic name shared with eight other modules would have applied one
    # module's declared options to all of them — and, one pin over, swept those
    # modules into the hand-composed-URL check for a fixture they never routed.
    # `NEST_BINARY_FIXTURES`' own comment already names this hazard; a
    # module-local fixture that routes needs a name only its module uses.
    #
    # That property is CHECKED now rather than asserted here (2026-09-02):
    # `test_nest_mode_axis.py::
    # test_a_shared_fixture_name_never_carries_two_different_option_sets`
    # derives the start options of every fixture definition in the tree —
    # including the bare-`start_nest` ones this table's own derivation does not
    # follow — and fails when one name carries two different sets. A shared name
    # whose definitions AGREE is fine and is not reported; three exist today and
    # cost nothing. It found `unclaimed_nest` disagreeing on its first run,
    # after a hand search had already found and fixed `handled_nest` and
    # stopped, which is the whole case for not leaving this to prose.
    "autoenable_nest": frozenset({"claim_domain", "serve_tls"}),
    "headless_unclaimed_nest": frozenset({"unclaimed"}),
    "rekey_unclaimed_nest": frozenset({"unclaimed"}),
    "self_signed_tls_nest": frozenset({"serve_tls", "unclaimed"}),
    "trust_resume_nest": frozenset(),
    "unclaimed_trust_nest": frozenset({"unclaimed"}),
    "windows_onboard_nest": frozenset({"serve_tls", "unclaimed"}),

    # The CRASH-RECOVERY pair, and the first fixtures to route on the strength
    # of a lifted absence rather than a met option. Their `extra_env` was always
    # honourable (the value is `RUST_LOG`, catalogued in `_DOCKER_EXTRA_ENV`);
    # what actually held them out was what their tests do with the handle
    # afterwards — eight of the module's eleven construct `NestLogWatch(nest)`,
    # which reads `log_path`, and that key was a declared docker absence until
    # 2026-09-02. The option table cannot see a read, which is exactly why the
    # blocker had to be measured rather than inferred from these two rows.
    "crash_nest": frozenset({"extra_env"}),
    "unclaimed_crash_nest": frozenset({"extra_env", "unclaimed"}),

    # The `handle_domain_seed` RESIDENTS — the first fixtures to route
    # *knowing they will never collect*, and the reason that is worth doing.
    # `handle_domain_seed` is the `--handle-domain` boot flag, standalone-only
    # and permanently so (ruling (3)), so routing these buys no test in a
    # container. What it buys is the ACCOUNT: unrouted, each was excluded by its
    # `nest_binary` closure — the MIXED class whose whole meaning is "closable
    # where the binary is incidental" — so the audit counted seven permanent
    # residents as outstanding work. Routed, each is excluded by the option it
    # actually cannot have, named in the reason a reader gets.
    #
    # Two populations, and ruling (3) names them both. Neither can reach the
    # domained claim (`claim_domain`), for two different and individually sound
    # reasons:
    #
    # * **An IP-literal authority** — the four `serve_tls` entries. The claim
    #   gate registers no local target (`claim_core.rs`), so `127.0.0.1:<port>`
    #   is not expressible as a claim at all. These pass
    #   `common.nest.OWN_DIAL_AUTHORITY` rather than a composed string: the
    #   authority is a fact about a nest that does not exist yet, and a fixture
    #   that had to allocate the port ITSELF in order to compose it is exactly a
    #   fixture that could not let a provider allocate it. That, rather than the
    #   option, is what had kept these four out of the seam.
    # * **An `unclaimed=True` nest** — the three below them. The domained claim
    #   needs a claim of the HARNESS's to ride on, and these fixtures exist
    #   precisely so the CLIENT makes that claim through the app UI. So the seed
    #   survives however registerable the value is (`MAIL_PRIMARY_DOMAIN` at all
    #   three), which is the measured refutation of the value-level
    #   registerable-vs-IP-literal split ruling (3) once implied: the
    #   discriminator is the sibling kwarg `unclaimed`, already name-level and
    #   already in this table, and the claimed-and-registerable cell is EMPTY.
    "caldav_cross_nest_peer": frozenset({"handle_domain_seed", "serve_tls"}),
    "cross_nest_foreign": frozenset({"handle_domain_seed", "serve_tls"}),
    "cross_nest_foreign_ephemeral": frozenset({"handle_domain_seed",
                                               "serve_tls"}),
    "smoke_handled_nest": frozenset({"handle_domain_seed", "serve_tls"}),
    "empty_store_unclaimed_nest": frozenset({"handle_domain_seed", "serve_tls",
                                             "unclaimed"}),
    "unclaimed_mail_nest": frozenset({"handle_domain_seed", "serve_tls",
                                      "unclaimed"}),
    "unclaimed_real_domain_nest": frozenset({"handle_domain_seed",
                                             "unclaimed"}),

    # The real-ambient-session category's one nest fixture (2026-09-02). Its
    # test's subject is the flatpak seam on the host — the systemd unit write,
    # enable, socket and uninstall condition-skip — and the nest exists only
    # because that write happens on the app's post-auth ensure path, so the app
    # has to sign in somewhere. Asking for nothing is therefore not a
    # coincidence: a URL and a claim is the entire contract, so the fixture
    # routes with no option to subtract and the run's app-vs-nest pairing
    # becomes the real one (a locally-built app artifact against the shipped
    # nest image) rather than two dev builds talking to each other.
    "throwaway_nest": frozenset(),

    # Module-local dedicated-nest fixtures added since the table was last
    # swept — recorded from the AST derivation itself
    # (`test_the_fixture_start_options_table_matches_the_tree`).
    "admin_succession_nest": frozenset(),
    "deletion_nest": frozenset(),
    "durable_nest": frozenset(),
    "index_nest": frozenset(),
    "push_nest": frozenset(),
    "restricted_nest": frozenset(),
    "routing_nest": frozenset({"claim_domain"}),
    "stalling_nest": frozenset(),
    "succession_nest": frozenset(),
    "tls_nest_with_registered_actor": frozenset({"serve_tls"}),
    "trust_destination": frozenset(),
    "trust_grant_nest": frozenset(),
    "unclaimed_dns_nest": frozenset({"unclaimed"}),
    "unclaimed_tls_nest": frozenset({"serve_tls", "unclaimed"}),
}

_UNSUPPORTED_OPTION_REASON = (
    "{fixtures} {verb} the nest with {options}, which nest mode {mode!r} does "
    "not honour YET ({have}) — testing.md § Default app and nest mode, ruling "
    "(3): an option the provider cannot honour is a declared absence, never a "
    "mounted config file and never a new entrypoint env. Closable: growing this "
    "provider's `supported_options` un-excludes every fixture needing only what "
    "it then supports, with no table edit"
)

_PERMANENT_OPTION_REASON = (
    "{fixtures} {verb} the nest with {options}, which nest mode {mode!r} will "
    "NEVER honour ({have}) — the provider says so itself "
    "(`permanently_unsupported_options`), so this is a fact about the mode and "
    "not a seam anyone can build. `handle_domain_seed` is the `--handle-domain` "
    "boot seed, and the only ways an image could take one are a mounted "
    "nest.toml or a new entrypoint env, both banned by name; in live the "
    "harness started no nest at all, so no start option is honourable there "
    "ever. testing.md § Default app and nest mode, ruling (3)"
)

#: Fixture → the MAIL-VENUE options it asks `_start_mail_venue` for. Ruling (3)'s
#: venue seam, classifier half — the exact twin of `FIXTURE_START_OPTIONS` above,
#: subtracted from the provider's own `supported_venue_options`.
#:
#: **Why a second table rather than a row in the first.** A mail venue is a
#: provider METHOD, not a start option, because its mail listeners must be
#: published at container start (arm 6; `conftest.py::_start_mail_venue` carries
#: the argument). So its options are a different vocabulary answered by a
#: different declaration, and conflating them would make a docker provider that
#: grows `caldav_only` look as though it had grown a *nest* option.
#:
#: A fixture that routes through the venue seam and asks for NOTHING still
#: belongs here, with an empty set: presence is what says "this fixture is a mail
#: venue", and absence is what the AST pin reads as a fixture that never routed.
#: The same lesson `FIXTURE_START_OPTIONS` records one table up.
MAIL_VENUE_FIXTURE_OPTIONS = {
    "dedicated_mail_nest": frozenset(),
    "dedicated_mail_nest_handle_domain": frozenset(),
    "dedicated_caldav_mailbox_less_nest": frozenset({"registration_open"}),
    "dedicated_caldav_only_nest": frozenset({"registration_open",
                                             "caldav_only"}),
}

_UNSUPPORTED_VENUE_REASON = (
    "{fixtures} {verb} a dedicated MAIL VENUE with {options}, a venue shape nest "
    "mode {mode!r} cannot stand up ({have}) — testing.md § Default app and nest "
    "mode, ruling (3): the docker shape of a mail nest is the image's own s6 "
    "bridges enabled through the product's toggle, and a venue option that shape "
    "cannot express is a declared absence"
)

#: The test functions that read a HOST-SPAWN affordance off their mail venue —
#: the exact map, with a disposition each, in the idiom this row already uses for
#: `_NODE_BINARY_CONSUMERS`. (It once named a `log_path` reader map as a third
#: instance; that map was retired 2026-09-02 when docker learned to publish
#: `log_path`, which is the outcome a per-test map is always second-best to —
#: see `test_nest_mode_axis.py`, where the retirement note stands.)
#:
#: Ruling (3) names the host-spawn affordances as docker's declared absences:
#: `mda` (for `respawn()` — s6 owns the rebind in the image), the two bridges'
#: log FILES, the metrics port. Measurement adds two of the same kind: `stub_mx`
#: (an in-process SMTP sink the harness spawns, reached through the
#: operator-hatch MX override the image's bridges do not read) and the DKIM pair
#: the standalone fixture reads from the nest.
#:
#: **This map is why the venue could be routed at all.** The fixture is a
#: capability question and these reads are a per-TEST one: three of the four
#: functions below sit in files whose other tests route perfectly well, so
#: excluding by fixture would have thrown away most of the arm's gain, and
#: excluding by nothing would have let a routed test reach a container and die on
#: an `AttributeError` in its own body — the "red against working product code"
#: outcome the arm ordering exists to prevent.
#:
#: **Only ROUTED venues are in scope**, and the pin enforces it in both
#: directions: `test_caldav_admin_port_rebind.py`'s `mda.respawn()` reader was
#: written in here first and the staleness arm rejected it, correctly — its
#: fixture never routed, so it is already excluded for the more basic reason
#: `LOCAL_NEST_FIXTURES` records, and a second entry here would have read as a
#: boundary where it is really a backlog line someone had already closed.
#:
#: Keyed `<file>::<test function>` and pinned by
#: `test_nest_mode_axis.py::test_mail_venue_host_affordance_readers_are_exactly_pinned`,
#: which re-derives the population by AST: a new reader is an edit someone has to
#: justify, and a reader that goes away must leave.
#: (The attribute names themselves are the PIN's business, not the classifier's:
#: they live beside `test_mail_venue_host_affordance_readers_are_exactly_pinned`
#: in `test_nest_mode_axis.py`, which is also the only file the raw-read scan
#: skips. Naming them here would make this module look like a reader of them.)
MAIL_VENUE_HOST_AFFORDANCE_READERS = {
    "tests/test_mail_enable_then_mua_round_trip.py::"
    "test_client_enabled_mail_submits_outbound_through_submission":
        "relays to the in-process stub MX and dkimpy-verifies the signature "
        "against the record the fixture read from the nest",
    "tests/test_mail_send_external_and_imap_auth.py::"
    "test_one_credential_sends_to_external_and_imap_authenticates":
        "reads the relayed message out of the in-process stub MX",
    "tests/test_tui_mail_outbound_from.py::"
    "test_tui_outbound_from_is_handle_at_handle_domain":
        "reads the relayed message out of the in-process stub MX",
}

_VENUE_HOST_AFFORDANCE_REASON = (
    "{disposition} — a HOST-SPAWN affordance of its mail venue, which nest mode "
    "{mode!r} declares absent (testing.md § Default app and nest mode, ruling "
    "(3)): the image's bridges are s6 services, so there is no harness-held "
    "process to respawn, no host log path, and no operator-hatch mta_mx_override "
    "routing to an in-process stub MX. The fixture itself routes; this one test "
    "does not"
)

#: The fixtures that compile the local `fauna-nest`. A test whose fixture
#: closure reaches one wants the BINARY — it stands up a nest of its own — and is
#: therefore standalone-only by construction, exactly like `LOCAL_NEST_FIXTURES`
#: but without needing to know the fixture's name.
#:
#: This is the general form of that list, and it exists because the list could
#: only ever see *conftest*. A dedicated-nest fixture defined in a test module —
#: `sell_nest`, `subs_nest`, `crash_nest`, `two_nodes`, ~100 of them across
#: `tests/`, `tests/api/` and `tests/platform/` — is invisible to it, so in a
#: docker or live run every one of those tests would have spawned a locally-built
#: nest while the report said docker/live. `item.fixturenames` is the TRANSITIVE
#: closure, so a module-local fixture requesting `nest_binary` puts that name in
#: its tests' closure and this catches it with no table at all.
#:
#: It only became possible once `nest_instance` stopped declaring `nest_binary`
#: and started resolving it lazily per mode (conftest): while every nest-using
#: test carried the name, matching on it would have excluded the entire suite —
#: which is why slice 2 had to hand-list conftest's fixtures instead.
#: The three names below are the OTHER half of the same hole, and the closure rule
#: cannot see them for a different reason: these fixtures do not *request* a binary
#: fixture, they COMPILE the nest themselves through a helper
#: (`helpers.ap_nest.build_ap_nest_binary`, a private `_bring_up_stack`). Nothing
#: then puts a binary name in their tests' closure, so before 2026-08-28 a docker
#: run SELECTED them, spent minutes on `cargo build -p fauna-nest` inside the run,
#: and would have stamped `fediverse` — a real catalog feature — as witnessed
#: against an image that never served the test. `test_nest_mode_axis.py`'s
#: `test_every_fixture_that_COMPILES_a_nest_is_named_in_the_binary_set` now derives
#: this class by AST (transitively within the module) instead of trusting a reader,
#: and it found all three on its first run — including `bluesky_node_binary`, one
#: letter from the `bluesky_nest_binary` already listed here.
NEST_BINARY_FIXTURES = frozenset({
    "nest_binary",
    "node_binary",          # tests/platform/conftest.py's name for the same build
    "bluesky_nest_binary",
    "bridges_nest_binary",
    "bench_nest_binary",
    "ap_binary",            # builds fauna-nest --features activitypub,test-hooks
    # `bluesky_node_binary` was here until 2026-09-02, when it was CONSOLIDATED
    # into `bluesky_nest_binary` rather than merely dropped: the two built nearly
    # the same nest (`bluesky` vs `test-hooks,nostr,bluesky`) for the same reason,
    # and the only thing that had kept them apart was `test_bluesky_oauth.py`
    # spawning its own nest by hand to pass `--bluesky-public-url`. That flag is
    # deleted — the OAuth client derives from the claimed identity domain — so
    # those tests now take ordinary `_make_nest` fixtures off the shared binary,
    # and one fewer nest gets compiled per run.
    "peer",                 # fediverse conftest, via _bring_up_stack
})

#: The nest feature set the SHIPPED image is built with, as the `Dockerfile`
#: spells it. Not a constant a reader should trust: `test_nest_mode_axis.py`
#: parses the `Dockerfile`'s own `--features` argv and asserts equality, so the
#: day the image gains or loses a provider this goes red instead of leaving the
#: two tables below quietly wrong.
IMAGE_NEST_FEATURES = frozenset({"bluesky", "nostr", "activitypub"})

#: Class (9) — `testing.md` § Default app and nest mode, exclusion class (9),
#: ratified 2026-09-02. Fixture → the exact `--features` string it builds, for
#: the fixtures whose TEST depends on the nest having been compiled that way.
#:
#: **The discriminator is not "a non-release feature set", and getting that
#: wrong is the easy mistake** — the arm that commissioned this class made it in
#: its own scoping note. EVERY binary fixture builds a non-release feature set,
#: `nest_binary` included (`build_node`'s default is `test-hooks,nostr` against
#: the image's `bluesky,nostr,activitypub`), so that phrase taken literally
#: classifies the whole suite out and dissolves the routing arm rather than
#: trimming it. What decides is whether the test DEPENDS on the difference.
#:
#: Today exactly one dependence qualifies, and it is `test-hooks` being
#: **compiled in** rather than any route being reachable — which is why this is
#: not `RULE_TEST_HOOKS` (that rule matches the `/api/v1/test/` prefix, and
#: these tests never call one). AP's outbound SSRF guard exempts a loopback
#: target only under `#[cfg(feature = "test-hooks")]` and only with
#: `FAUNA_TEST_AP_ALLOW_LOOPBACK` set
#: (`bins/fauna-nest/src/activitypub/outbound.rs::test_loopback_allowed`), and
#: every AP peer this harness can host is at loopback: the federation suite's
#: in-process peers directly, and the fediverse interop stack through
#: Mastodon's loopback-published TLS front. A release image can therefore never
#: serve either, in any mode, forever — a FACT, exactly like class (6) one
#: artifact-property along.
FEATURE_SET_BINARY_FIXTURES = {
    "ap_binary": "activitypub,test-hooks",
    "peer": "activitypub,test-hooks",  # fediverse conftest, via _bring_up_stack
}

#: The other half of the same pin, and the reason it is a table rather than a
#: comment: a fixture that names an explicit feature set and is NOT in the class
#: has to say why, or the next reader re-derives the question from scratch.
#:
#: Fixture → (features string, why the run's image can serve it anyway). Each of
#: these stays ordinary `nest_binary` debt — closable, and worth a row — rather
#: than becoming a fact about the artifact.
IMAGE_SERVABLE_BINARY_FIXTURES = {
    "bluesky_nest_binary": (
        "test-hooks,nostr,bluesky",
        "the capability it pins is the `bluesky` cargo feature (the full-PDS write kind registers "
        "under `#[cfg(feature = \"bluesky\")]`), which the image ships; the "
        "`test-hooks` in the string is the e2e baseline riding along, not "
        "something these tests reach",
    ),
    "bridges_nest_binary": (
        "test-hooks,activitypub",
        "the provider its Bridges-page journeys link is ActivityPub, which "
        "registers under `#[cfg(feature = \"activitypub\")]` and which the image "
        "ships. The image's other two providers never reach that page: all 7 "
        "apps filter it through the shared `is_unified_bridges_page_bridge`, "
        "which drops `nostr` and `bluesky`, so a container renders the same one "
        "card. The narrow build is a standalone build-time saving, not a premise",
    ),
}

#: Dedicated-nest fixture → the BUILD FIXTURE it names to the provider seam
#: (`_start_dedicated_nest(…, binary="<name>")`), for the fixtures whose test
#: needs a provider the default `nest_binary` does not compile. `testing.md`
#: § Default app and nest mode, ruling (1), added 2026-10-05.
#:
#: The name travels as a string rather than a fixture parameter for the same
#: reason `nest_binary` is resolved lazily: a declared parameter puts the
#: build in every mode's fixture closure, where the classifier reads it as the
#: `nest_binary` exclusion even though docker serves the image. So the name is
#: invisible to `item.fixturenames`, and this table is what makes it visible
#: again: the collection-time prebuild reads it to build the binary before any
#: test's clock starts, and the seam reads it to refuse, in a non-local mode,
#: a name the image cannot stand in for. `test_nest_mode_axis.py` pins it to
#: the literal `binary=` each fixture passes, and every value must be an
#: `IMAGE_SERVABLE_BINARY_FIXTURES` key. A class (9) build would be the
#: harness lying about which artifact ran.
DEDICATED_NEST_BINARIES = {
    "domained_bluesky_nest": "bluesky_nest_binary",
    "domainless_bluesky_nest": "bluesky_nest_binary",
    "bridges_nest": "bridges_nest_binary",
}

_FEATURE_SET_BINARY_REASON = (
    "depends on {fixtures}, which builds fauna-nest `--features {features}`: the "
    "test needs a nest COMPILED with `test-hooks`, not merely a route under "
    "/api/v1/test/ — AP's outbound SSRF guard exempts the loopback peers this "
    "harness hosts only under `#[cfg(feature = \"test-hooks\")]` "
    "(activitypub/outbound.rs::test_loopback_allowed). A release artifact "
    "carries no test-hooks (convention 15), so {mode!r} can never serve it: a "
    "fact about the artifact, not a gap to close (testing.md § Default app and "
    "nest mode — exclusion class (9))"
)

_NEST_BINARY_REASON = (
    "depends on {fixtures}, i.e. on the locally-COMPILED fauna-nest: the test "
    "stands up a nest of its own, so in this mode it would serve a local binary "
    "while the run reported {mode!r} (testing.md § Default app and nest mode — "
    "exclusion classes (1) fresh-state and (2) nest-side instrumentation)"
)

#: The tag every `tests/platform/docker/` fixture boots, through
#: `tests/platform/docker/helpers.py::docker_build`. Named here so the deselect
#: reason can say *which* image the test brings, and so the remedy below is one
#: copy-pasteable line rather than a hunt.
OWN_IMAGE_TAG = "fauna-nest-test:local"

#: Fixtures that stand up a nest from their OWN container IMAGE — the image twin
#: of `NEST_BINARY_FIXTURES`, and the same defect one artifact along.
#:
#: `tests/platform/docker/` is a self-contained tier_4 venue: every fixture here
#: boots `fauna-nest-test:local` — through `docker_build`, which since
#: 2026-08-28 reuses whatever image carries that tag and refuses to build one,
#: or through the container-start helpers that default to it. The run's
#: `--nest` flag neither configures that container nor describes it, so such a
#: test is **not a witness of the run's nest in any mode** — which would make all
#: three of the ledger's run-level provenance fields false at once: `commit` (the
#: image was built at some other commit), `image_digest` (the run's `--nest
#: docker:<ref>` digest, not this container's), and `nest_mode` (`standalone`
#: under the default inner loop, for a test that ran against an image).
#: `feature-catalog.md` § The ledger licenses a record *because* it is
#: first-party "on the commit it names, against the artifact it names"; so the
#: writer records such a test against the container it actually booted
#: (`feature_ledger.note_own_artifact` → `own_artifact_resolution`, fed by the
#: package's `wait_for_health`) and the axis deselects it from docker and live
#: runs, whose whole claim is "this exact image".
#:
#: **Measured 2026-08-29, and the cost was not hypothetical.** A docker-mode
#: `--feature` sweep SELECTED 81 of this package's 89 tests, and 11 of them died
#: `fauna.auth.signature_failed` — recorded by the sweep as an unexplained tail.
#: The cause was the tag: `fauna-nest-test:local` on that box had been built
#: 2026-08-14, three days before the change ("the actor key signs tagged-only
#: — transition machinery deleted"), so a current-tree client's tagged signature
#: is one the image's nest cannot verify. Controlled differential: the `tests/api/`
#: slice is 107 passed / 0 failed against `ghcr.io/faunasocial/nest:latest` and
#: **108 errors, every one `fauna.auth.signature_failed`**, against that same
#: stale tag — same tests, same commit, same mode, only the image differs. With
#: the ledger on (its default) those 11 would have written ❌ cells accusing
#: working product code, stamped with a commit the image predates.
#:
#: Derived by AST from the package rather than hand-read, transitively and
#: across module boundaries, by `test_nest_mode_axis.py::
#: test_the_own_image_fixture_list_matches_the_platform_docker_package`.
#:
#: **`nest` is deliberately absent, and that absence is checked.** Matching is by
#: fixture NAME, and this module's docstring warns that a name shared with a
#: fixture elsewhere would deselect strangers with a plausible-reading reason —
#: `nest` is defined in `tests/api/test_invite_requests.py` and
#: `tests/test_ap_nest_readers.py` too, reaching 30 tests outside this package.
#: Dropping it costs nothing because both of the package's `nest` fixtures
#: *request* `docker_image`, which is in the transitive closure anyway; the pin
#: asserts that redundancy instead of trusting it, and a second pin fails if any
#: name in this set ever starts colliding.
OWN_IMAGE_FIXTURES = frozenset({
    "accepting_nest",
    "acme_stack",
    "auto_dkim_nest",
    "autosched_nest",
    "bare_ip_caldav_nest",
    "bidi_nest",
    "bidi_sidecars",
    "booted_nest",
    "booted_nest_mail_enabled",
    "claimed_nest",
    "dane_nest",
    "docker_image",
    "docker_mail_nest",
    "domained_nest",
    "docker_nest",
    "docker_reclaim_nest",
    "domained_claim_stack",
    "domainless_stack",
    "env_claim_nest",
    "family_nest",
    "forwarder_ndr_nest",
    "lenient_nest",
    "mail_serving_nest",
    "mounted_claim_nest",
    "mta_sts_nest",
    "ndr_sidecars",
    "nest_container",
    "outbound_nest",
    "real_clamd_nest",
    "relay_nest",
    "relay_net",
    "round_trip_nest",
    "scanners",
    "self_signed_floor_nest",
    "serving_atproto",
    "serving_nest",
    "shipped_nest",
    "site_nest",
    "smtp_relay_net",
    "spoofing_nest",
    "stub_mx",
    "topology",
    "two_boxes",
})

_OWN_IMAGE_REASON = (
    "depends on {fixtures}, which {verb} a nest from the test's OWN image "
    f"({OWN_IMAGE_TAG}): the run's nest mode neither configures that container "
    "nor describes it, so in {mode!r} the test would witness an artifact the run "
    "never named — its ledger record names the container it booted, never the "
    "run's commit, image digest or mode (feature-catalog.md § The ledger, "
    "own-artifact records). tests/platform/docker/ is its own tier_4 "
    "venue: run it with `just e2e-tier-4-test`, and keep the tag current with "
    f"`docker pull ghcr.io/faunasocial/nest:latest && docker tag "
    f"ghcr.io/faunasocial/nest:latest {OWN_IMAGE_TAG}`"
)

#: Fixtures that spawn a bridge process **on the host** — the mail MTA/MDA
#: bridges the harness runs beside the nest, wired up through conftest's
#: `_spawn_{mta,mda}_bridge`.
#:
#: These are standalone-only for a reason that is neither of the two above, and
#: unlike those it is a fact about the PRODUCT rather than about the harness:
#: `fauna.bridges.request_enrollment` is **loopback-gated** at the dispatcher
#: (`routes.rs` step (1c) → `pre_identity_allowlist::requires_loopback_peer`),
#: and the gate fails CLOSED when `peer_addr` is `None`. A bridge process on the
#: host that reaches a containerised nest through its published port is not a
#: loopback peer — nest sees the docker gateway address, the very fact
#: `test_caldav_sni_router.py::test_external_request_enrollment_refused_over_router`
#: asserts — so the bridge is refused with
#: `fauna.bridges.remote_enrollment_unsupported`, never opens its listeners, and
#: every test in the module times out waiting for `/healthz`. Against a live box
#: the bridge is not even on the same machine.
#:
#: Cross-IP bridge enrollment is **deliberately deferred** until a multi-host
#: deployment needs it (`mail-bridge-lifecycle.md` § Cold boot step 3), so this
#: is a classification, not a bug to fix: the faithful docker shape for mail is
#: the image's OWN s6-supervised bridges dialling the container's loopback,
#: which `tests/platform/docker/` already drives.
#:
#: Measured 2026-08-28 (the second docker-mode sweep): six mail modules failed
#: with zero passes each — `test_mail_bridge_mta.py` 26 outcomes,
#: `test_mail_bridge_mda.py` 13, and four more — all on a 30 s `/healthz`
#: timeout whose cause is this refusal. Derived by AST, not hand-read, by
#: `test_nest_mode_axis.py::
#: test_every_fixture_that_SPAWNS_A_BRIDGE_is_named_in_the_bridge_set`.
#: Only the ones no other class already catches: most bridge-spawning fixtures
#: also stand up a nest of their own and are excluded by `LOCAL_NEST_FIXTURES`
#: for that more basic reason. The pin asserts the union covers them all.
BRIDGE_SPAWN_FIXTURES = frozenset({
    "mail_bridge_mta",
    "mail_bridge_mda",
    "disposable_mta_bridge",
    "disposable_mda_bridge",
    "mail_bridge_inbound_to_imap",
    "unclaimed_caldav_nest",
    "autoenable_mda",       # module-local: tests/test_mail_auto_enable_first_setup.py
})

#: Class (5)'s THIRD door: the fixtures that BUILD a self-enrolling bridge.
#:
#: The set above catches a bridge spawned by a FIXTURE, and `LOOPBACK_GATED_KINDS`
#: below catches a body that drives the enrollment kind over its own WS client.
#: Neither sees the shape measured 2026-09-02: a test body that
#: `subprocess.Popen`s the built bridge binary INLINE
#: (`test_atproto_bridge_enroll.py:78` is the canonical one; seven ATProto and
#: Bluesky bodies do it). There is no spawning fixture to name, and the kind is
#: never reached from Python at all — the bridge, a separate process, is the one
#: that calls it.
#:
#: The signal is the BINARY fixture, one AST hop ahead of the `Popen`: a test
#: asks for a built bridge binary in order to RUN one, there being nothing else
#: to do with it. That is a stronger premise than it looks, and it is pinned to
#: the product rather than asserted — a fixture belongs here exactly when the
#: binary it builds reaches `wsrpc.EnrollAndAwaitApproval`, which is why
#: `seal_helper_binary` (same `bins/fauna-bridges/` tree, a client-side seal
#: step, no enrollment) is deliberately absent. See `test_nest_mode_axis.py::
#: test_the_bridge_BINARY_fixture_set_matches_the_bridges_that_SELF_ENROLL`.
#:
#: What this changes is the ACCOUNT, not the run. Every one of these tests also
#: builds a local nest, so `classify`'s first match already excluded them and
#: no test moves mode. But the audit reads `all_rules`, and the ratified order
#: puts `nest_binary` (MIXED — closable where the binary is incidental) ahead of
#: `bridge_spawn` (a FACT), so until this door existed all seven were filed as
#: closable routing work. Routing them would have turned seven clean
#: collection-time exclusions into seven runtime ❌ against product code
#: behaving exactly as ratified — the third arrival of the overstatement
#: `_verdicts`' own docstring names, through a door that doc had not considered.
BRIDGE_BINARY_FIXTURES = frozenset({
    "mail_bridge_binary",
    "atproto_bridge_binary",
    "atproto_bridge_e2e_binary",
})

#: The two evidence clauses, and the one tail they share. Kept apart because
#: the verb differs and the difference is load-bearing: a spawn fixture RUNS
#: the bridge, a binary fixture only BUILDS it and the test body runs it, and a
#: reason claiming the wrong one sends its reader looking for a fixture that
#: does not exist.
_BRIDGE_SPAWN_CLAUSE = "depends on {fixtures}, which {verb} a bridge process"
_BRIDGE_BINARY_CLAUSE = (
    "requests {fixtures} — the BUILT bridge binary — and spawns it in its "
    "own test body"
)

_BRIDGE_SPAWN_REASON = (
    "{evidence} on the HOST: "
    "`fauna.bridges.request_enrollment` is loopback-gated (a host process "
    "reaching a container's published port, or a live box on another machine, "
    "is not a loopback peer), so the bridge is refused "
    "`fauna.bridges.remote_enrollment_unsupported` and never serves. Cross-IP "
    "bridge enrollment is deliberately deferred (mail-bridge-lifecycle.md "
    "§ Cold boot step 3); the faithful {mode!r} shape for mail is the image's "
    "own supervised bridges, which tests/platform/docker/ drives"
)

#: The kinds nest refuses from a non-loopback peer — class (5)'s BODY half.
#:
#: The fixture set above catches a test that gets a bridge process spawned FOR
#: it. It cannot catch one that drives the enrollment kind itself over an
#: anonymous WS client, which is what `test_mail_bridge_approval.py::
#: test_request_enrollment_auto_approves_when_mail_enabled` does — no bridge
#: fixture anywhere in its closure, and in docker mode it failed on the very
#: `fauna.bridges.remote_enrollment_unsupported` the fixture reason above
#: describes (measured 2026-08-29). Exactly the gap class (3) already had and
#: closed with a call-graph scan, so it is closed the same way.
#:
#: Pinned BY EQUALITY to nest's own gate — `pre_identity_allowlist.rs::
#: requires_loopback_peer` — rather than restated here, so a second
#: loopback-gated kind cannot appear on one side only.
LOOPBACK_GATED_KINDS = frozenset({
    "fauna.bridges.request_enrollment",
})

_LOOPBACK_KIND_REASON = (
    "reaches {kinds}, which nest refuses from a non-loopback peer: in {mode!r} "
    "the nest is a container (the harness sees the docker gateway address) or a "
    "box on another machine, so the call comes back "
    "`fauna.bridges.remote_enrollment_unsupported`. Cross-IP bridge enrollment "
    "is deliberately deferred (mail-bridge-lifecycle.md § Cold boot step 3), so "
    "this is a classification and not a gap to close"
)

#: Class (8): a test that hands ONE nest the address of ANOTHER.
#:
#: The product refuses it, and the refusal is the security posture working
#: rather than a harness gap. `federation_channel::validate_peer_url` carves out
#: loopback *literals* for in-process and tier_3 peers and otherwise requires a
#: globally routable https target; a container network is RFC1918 by
#: construction, so in docker mode nest A dialing nest B is refused `NonGlobal`.
#: Production peers are globally routable, which is why this is a classification
#: and not a bug to fix (`testing.md` § Default app and nest mode, ruling (2)).
#:
#: **Why this class is a KEY READ and not a kind set, unlike class (5).** Class
#: (5) pins to `requires_loopback_peer` because nest gates *by kind*, at the
#: dispatcher, before the handler runs. Class (8)'s guard fires on the
#: **address**, inside the federation pool, below the kind layer — and for an
#: *enrolling* kind (`fauna.feed.contributors.grant` stores the peer URL and a
#: background worker dials it minutes later) it fires in a worker, with the
#: refusal absorbed as a row delete the caller never sees. So there is no
#: kind-shaped fact in nest to pin to, and four candidate instruments were built
#: and measured away before this one; the goal doc records them so the next
#: session does not re-walk them. The two that look most plausible from here:
#: the peer-authority REQUEST FIELD name does not classify, because `nest_url`
#: is also this harness's ordinary parameter name for *the nest I am dialing*
#: (`ApiActor(nest_url=second_nest["url"])`, `put_blob(nest_url, ...)`) — the
#: same word, opposite meanings on the two sides of the wire; and kind-reach
#: alone scores ZERO on every real witness while admitting ~20 same-nest
#: round-trips whose relay field is simply absent.
#:
#: What remains is the harness's own act: after ruling (2)'s transport half,
#: taking one nest's authority to hand to another has exactly one spelling —
#: reading `peer_url` off a handle. A client dialing its own nest reads `url`,
#: which is why the two could not stay aliases. The read is what counts, never a
#: mention: the docker provider *writes* `{"peer_url": ...}` inside a method
#: named `start`, and since the reach graph is keyed on the bare function name,
#: counting writes floods 3428 of ~4200 test functions (measured). `_key_reads`
#: takes subscript **loads** only, which is the act and not the furnishing.
PEER_AUTHORITY_KEYS = frozenset({"peer_url"})

#: The fixtures that put a nest in a test's hands. Class (8) is meaningless
#: without one: a test that starts no nest cannot make nest A dial nest B, no
#: matter which keys its assertions read. Unioned from the harness's own
#: inventory of nest-starting fixtures rather than restated, plus the shared
#: nest every ordinary test rides.
#:
#: ⚠ ``nest_mode`` is the STRUCTURAL member and it is what makes this gate
#: survive arm 1. The other three sets all mean *this fixture has not been
#: ROUTED yet* — a local-binary name, a hand-listed conftest fixture, the
#: session nest — and routing is precisely the act of leaving them. So without
#: ``nest_mode`` the gate went blind at the exact instant class (8) started
#: mattering: a routed nest-DIALS-nest fixture produced an empty intersection,
#: its tests were admitted to docker, and the product refused the dial there —
#: silently for the enrolling half (``fauna.feed.contributors.grant`` upserts
#: the row and the discovery worker's refusal is absorbed minutes later as a
#: failure count and a row DELETE), so the ❌ would have been an undiagnosable
#: timeout. ``nest_mode`` cannot be avoided by a fixture that starts a
#: harness-owned nest: it has to hand it to ``_start_dedicated_nest`` to pick a
#: provider. This is the same structural member ``_LOCAL_NEST_FIXTURE_USERS``
#: adopted for the prebuild trigger on 2026-08-30, after the identical
#: hand-maintained-list failure one artifact along.
_NEST_BEARING_FIXTURES = (
    LOCAL_NEST_FIXTURES
    | NEST_BINARY_FIXTURES
    | frozenset({"nest_instance", "nest_mode"})
)

_PRIVATE_PEER_REASON = (
    "hands one nest the address of another (it reads the {keys} contract key), "
    "which the product refuses in {mode!r}: the peer a container reaches is on "
    "the run's user-defined network, and `federation_channel::validate_peer_url` "
    "carves out loopback literals only and otherwise requires a globally "
    "routable https target, so the dial comes back `NonGlobal`. Production peers "
    "ARE globally routable — this is the SSRF posture working, not a gap to "
    "close (testing.md § Default app and nest mode, ruling (2)). **Measured "
    "2026-08-29 against two real containers**, and the second half of that "
    "measurement is why this class must exclude at COLLECTION rather than let "
    "the test run and read the failure: the nest logs `federation peer URL "
    "resolves to a non-global address: https://172.18.0.3:3000`, but the CLIENT "
    "is told only `fauna.folders.internal` / `federation fetch failed` — so a "
    "test could not distinguish this refusal from a broken network, a dead peer "
    "or a TLS fault, and would report a red of unknown cause"
)

#: Class (6): the test-hooks HTTP surface, which a RELEASE artifact does not have.
#:
#: Convention 15 (`testing.md`) compiles the automation surface out of release
#: artifacts, and nest implements that as `#![cfg(feature = "test-hooks")]` on
#: every `*_test_hook.rs` module — "Production never compiles this module", in
#: `web_paywall_test_hook.rs`'s own words. The published image and a live box are
#: both release builds, so every route under this prefix answers **404** there.
#:
#: That is a fact about the ARTIFACT, not a harness gap and not a product defect,
#: so it is a classification in exactly the sense class (5) is. Measured
#: 2026-08-29: `test_web_paywall_folder_expired_token_serves_teaser` was one of
#: three survivors of the docker-mode `tests/api/` slice, failing
#: `HTTPError: 404` on `/api/v1/test/web-paywall/expired-token` — a ❌ that would
#: have accused `web-paywall` of being broken in the image it is fine in.
#:
#: A prefix, not a route list: the harness builds these URLs by f-string and some
#: carry path parameters, so no whole-route literal exists to match against. The
#: prefix is safe because nest serves nothing else under it —
#: `test_the_test_hook_prefix_covers_every_test_hook_route_nest_registers` pins
#: that against the Rust modules by equality.
#:
#: ⚠ **This class catches only the ROUTE half of what `test-hooks` changes.** The
#: same flag also swaps **compile-time constants** in modules that ship either
#: way — `anonymous_rate_limit.rs`'s `DISCOVERY_MAX_EVENTS` / `REGISTER_MAX_EVENTS`
#: are 60 / 10 in a release build and 100_000 under `test-hooks` — and such a test
#: reaches no test-hook route, so no prefix match can see it.
#:
#: **That half is deliberately NOT a class here, and must not become one.**
#: `testing.md` § Default app and nest mode rules it (*"a shipped limit is a test
#: constraint, not a route into that classification"*, ratified 2026-08-30): a
#: journey test that merely *spends* a shipped budget has to FIT it, and turning
#: that into an inferred exclusion would blank cells that are coverage holes. Only
#: a test whose **subject is the budget** may opt out, and it says so itself with
#: `@pytest.mark.standalone_only` — still a tallied `DECLARED_ABSENCE`
#: (`_verdicts`' first rule), just hand-placed, because the dependency lives in
#: what the test *asserts* rather than in any URL, fixture or kind it touches.
#: Worked example of each side: `tests/api/test_discovery_rate_budget.py` is the
#: subject case (marked); `tests/test_family.py` was the spender that was made to
#: cost 6 instead of 30. 
TEST_HOOK_ROUTE_PREFIX = "/api/v1/test/"

_TEST_HOOK_REASON = (
    "reaches the test-hooks HTTP surface ({routes}), which a RELEASE artifact "
    "does not carry: nest gates every *_test_hook.rs module on "
    "`#![cfg(feature = \"test-hooks\")]` (e2e conventions point 15 — the "
    "automation surface is compiled out of release artifacts), so in {mode!r} "
    "the route answers 404. A fact about the artifact, not a gap to close"
)

#: The WS-RPC twin of the route prefix: kinds only a `test-hooks` build
#: REGISTERS. Nest declares each one in a module `lib.rs` gates on
#: `#[cfg(feature = "test-hooks")]` (`fauna.protocol.echo`, in `protocol_test`),
#: so a release artifact answers it `fauna.protocol.unknown_kind` — measured on
#: dev.example.com 2026-10-05, where it was recorded as a failed live cell.
#: Matched as a closed vocabulary through the call graph, like class (3)'s
#: kinds (a kind is a rare string); pinned by equality to the string literals of
#: the gated modules in `test_nest_mode_axis.py`.
TEST_HOOK_KINDS = frozenset({"fauna.protocol.echo"})

_TEST_HOOK_KIND_REASON = (
    "calls {kinds}, which only a `test-hooks` build of nest registers (the "
    "module serving it is gated on `#[cfg(feature = \"test-hooks\")]`, e2e "
    "conventions point 15), so in {mode!r} a release artifact answers it "
    "`fauna.protocol.unknown_kind`. A fact about the artifact, not a gap to close"
)

#: Why each of the above is standalone-only, in the ratified classes' terms.
#: Class (1) fresh-state and class (2) nest-side instrumentation both land here;
#: the distinction is kept in the reason string so the tally stays readable.
_LOCAL_FIXTURE_REASON = (
    "depends on {fixtures}, which {verb} a local fauna-nest binary directly "
    "rather than through the mode provider (testing.md § Default app and nest "
    "mode — exclusion classes (1) fresh-state and (2) nest-side instrumentation)"
)

#: Class (3), **live only**: conftest fixtures that mutate nest state OUTSIDE the
#: run's own account — mail domains, spam policy, bridge service users. On a
#: throwaway nest that is free; on the shared live box it is exactly what the
#: shared-box rule forbids, because it changes what a human signed into that box
#: experiences.
#:
#: Derived, not hand-read: `test_nest_mode_axis.py` asks `kind_reach` (the graph
#: `classify` uses, over the e2e and `tests/common/` trees) which conftest
#: fixtures reach a kind outside `ACCOUNT_SCOPED_KINDS` below, and asserts
#: equality with this set (minus the ones `LOCAL_NEST_FIXTURES` already excludes
#: everywhere). Module-level fixtures elsewhere are on no list; `classify`'s
#: closure walk attributes their reach to the test directly. The
#: classification is a **deny-by-default over a closed protocol vocabulary**: an
#: admin kind nobody has classified counts as global, so a new one shrinks live's
#: eligible set until someone looks — the safe direction, and the opposite of the
#: `app_capabilities.py` failure this module exists to prevent.
GLOBAL_ADMIN_FIXTURES = frozenset({
    "caldav_cross_nest_peer",
    "cross_nest_foreign",
    "cross_nest_foreign_ephemeral",
    "dedicated_caldav_mailbox_less_nest",
    "dedicated_caldav_only_nest",
    "dedicated_mail_nest",
    "dedicated_mail_nest_handle_domain",
    "disposable_mda_bridge",
    "disposable_mta_bridge",
    "handled_nest",
    "mail_bridge_inbound_to_imap",
    "mail_bridge_mda",
    "mail_bridge_mta",
    "registration_posture_nest",
    "unclaimed_caldav_nest",
})

#: Fixtures that reach a global-admin kind but **disable themselves on live**, so
#: depending on one does NOT exclude a test. Each must stay exactly the kind of
#: thing that can self-gate, because excluding it would empty live's eligible
#: set: `_session_primary_mail_domain` is *autouse*, and `nest_instance` and
#: `test_user` sit under every `logged_in_app` test. Each skips its mutation on
#: live instead (conftest, gated on the run mode):
#:
#: * `_session_primary_mail_domain` returns early — a live box already has its
#:   primary domain.
#: * `nest_instance` refuses `--reclaim-cycle`, whose `fauna.admin.factory_reset`
#:   would wipe the real box.
#: * `test_user` leaves the `free` tier's caps alone: that row is every real
#:   user's tier and `tiers.update` has no account-scoped form, so the run's
#:   account takes the box's caps as they are. Its self-gate is pinned
#:   behaviourally, not just listed.
#:
#: Listed rather than inferred because "does this fixture check the mode?" is
#: not a question an AST walk answers honestly; the AST pin's job here is to
#: make sure a fixture cannot join this set silently.
LIVE_SELF_GATED_FIXTURES = frozenset({
    "_session_primary_mail_domain",
    "nest_instance",
    "test_user",
})

#: ── The kind partition ───────────────────────────────────────────────────
#:
#: Class (3) asks one question of a test: *can it mutate nest state outside the
#: run's own account?* The tractable way to answer it is to classify the
#: **kinds** — a closed vocabulary of 275 that `libs/fauna-protocol/src/kind.rs`
#: registers — rather than the open-ended set of tests that call them. Scanning
#: test bodies for a `fauna.admin.`/`fauna.bridges.` *prefix* was measured and
#: rejected: it matches 94 files, and its hits are heavily polluted by read-only
#: kinds and by **error codes that are not kinds at all**
#: (`fauna.bridges.permission_denied`, `fauna.admin.conflict`). Membership in
#: the partition below excludes those by construction, which is the whole reason
#: classifying kinds works where classifying tests does not.
#:
#: The split is **derived from the nest's own authorization boundary**, not from
#: a name heuristic, by three rules — anything they do not admit is global, so
#: the lazy direction is the safe one:
#:
#:   **R1 (account-data-plane.md § The ratified decisions)** `bins/fauna-nest/src/bridge_method_allowlist.rs::is_permitted` is a
#:   kind → `CallerClass` table covering 274 of the 275. A kind an ordinary
#:   `User`-class caller may invoke **cannot** mutate outside that caller's own
#:   account — otherwise the nest has a privilege bug — because every user-facing
#:   kind is caller-scoped (`operates on target == caller`, stated in that file's
#:   own doc comment). So the boundary the nest already enforces decides the
#:   partition. This is what correctly admits kinds whose *names* read nest-wide:
#:   `set_mail_serving_enabled` is "User-class, caller-scoped"
#:   (`bins/fauna-nest/src/db/mail_serving.rs:16`).
#:
#:   **R2** An `Admin`-class **read** is account-scoped: it mutates nothing, so
#:   it cannot mutate anything shared. This is the rule that stops the pollution
#:   — `get_mail_config`, `list_local_domains`, `list_service_users` and the
#:   `fauna.admin.*.list` reads are what test bodies name constantly, and
#:   excluding on them would gut live's eligible set for no reason.
#:
#:   **R3** A carve-out for the harness's own account provisioning
#:   (`users.{create,update,delete,suspend}`, `folders.{create,add_member}`),
#:   inherited from the 23-entry allowlist this partition replaces.
#:   ⚠ R3 is **argument-dependent, not per-kind safe**: `users.delete` is
#:   account-scoped only because the harness names *its own* actor. A test that
#:   passes someone else's actor id is mutating another account, and no static
#:   classification can see that — which is why these six are a stated carve-out
#:   rather than a derivation.
#:
#: ⚠ **Bridge-ROLE-class kinds are argument-scoped, not caller-scoped.** A kind
#: whose caller class is `BridgeMta`/`BridgeMda`/`BridgeAtprotoPds`/
#: `ContentProcessor` takes an explicit `actor_id`, so R1's reasoning does not
#: extend to it and a *read* of that class is not self-evidently harmless. They
#: are therefore all global here, read-shaped ones included. (This is about the
#: caller class, not the `fauna.bridges.*` namespace: that namespace also holds
#: admin-facing kinds, and R2 admits 12 of them as Admin-class reads.)
#:
#: That exclusion was measured before it was chosen: admitting the 23 read-shaped
#: bridge-role kinds excludes **exactly the same 149 tests**, because a test that
#: reaches one always reaches a global bridge kind too. It buys no coverage, and
#: it would require adjudicating kind by kind which read-shaped bridge kinds
#: secretly write — a judgement this session got wrong twice by trusting
#: documentation over handlers:
#:
#:   * `check_greylist` reads like a query and **upserts a nest-wide greylist
#:     row** (`bins/fauna-nest/src/bridge_routing_handlers.rs`, "read → decide →
#:     upsert round-trip with cross-call persistence").
#:   * `list_mailboxes` and `fetch_message_ciphertext` **seed the standard
#:     mailboxes on first touch** (`ensure_bridge_imap_mailboxes` +
#:     `emit_bootstrap_create_records`, `bridge_imap_handlers.rs`) — while
#:     `fetch_index_segments_since`, which `kind.rs`'s own comment names as one of
#:     the seeding four, does **not** seed.
#:
#: Both sets are pinned to `kind.rs` **by equality** in `test_nest_mode_axis.py`:
#: their union must be exactly the registered vocabulary, so registering a kind
#: without classifying it **fails a tier_1 test** until somebody does. That pin
#: is the mechanism, and it has to be, because the recognizer below is the
#: vocabulary itself: a kind in neither set is not matched in a test body at all
#: (matching by prefix instead is what would drag the error codes back in). So
#: the guarantee is "an unclassified kind stops the suite", not "an unclassified
#: kind is silently treated as global" — within the partition, though, `classify`
#: does test *non-membership of the account-scoped set*, so mis-filing a kind by
#: omission still lands on the safe side.
ACCOUNT_SCOPED_KINDS = frozenset({
    "fauna.admin.admins.list",
    "fauna.admin.audit.integrity",
    "fauna.admin.audit.list",
    "fauna.admin.cluster.status",
    "fauna.admin.deployment_seed.get",
    "fauna.admin.evictions.list",
    "fauna.admin.folders.add_member",
    "fauna.admin.folders.create",
    "fauna.admin.folders.get",
    "fauna.admin.invite_codes.list",
    "fauna.admin.invite_requests.list",
    "fauna.admin.logs",
    "fauna.admin.membership_tiers.list",
    "fauna.admin.pending_actions.list",
    "fauna.admin.region.get",
    "fauna.admin.services.list",
    "fauna.admin.stats",
    "fauna.admin.status",
    "fauna.admin.tiers.list",
    "fauna.admin.users.create",
    "fauna.admin.users.delete",
    "fauna.admin.users.get",
    "fauna.admin.users.list",
    "fauna.admin.users.suspend",
    "fauna.admin.users.update",
    "fauna.admin.web_app_origin.get",
    "fauna.admin.worker.status",
    "fauna.bridges.add_follow",
    "fauna.bridges.add_list_member",
    "fauna.bridges.atproto.delete_presence",
    "fauna.bridges.atproto.fetch_authoring_delegation",
    "fauna.bridges.atproto.fetch_authoring_key",
    "fauna.bridges.atproto.get_integration_status",
    "fauna.bridges.atproto.list_app_credentials",
    "fauna.bridges.atproto.list_grants",
    "fauna.bridges.atproto.list_pending_consents",
    "fauna.bridges.atproto.list_sessions",
    "fauna.bridges.atproto.provision_app_credential",
    "fauna.bridges.atproto.provision_authoring_delegation",
    "fauna.bridges.atproto.record_tombstone",
    "fauna.bridges.atproto.request_tombstone",
    "fauna.bridges.atproto.resolve_consent",
    "fauna.bridges.atproto.revoke_app_credential",
    "fauna.bridges.atproto.revoke_authoring_delegation",
    "fauna.bridges.atproto.revoke_session",
    "fauna.bridges.atproto.set_external_apps_enabled",
    "fauna.bridges.atproto.set_integration_level",
    "fauna.bridges.batch_import_list_members",
    "fauna.bridges.cancel_export_session",
    "fauna.bridges.cancel_import_session",
    "fauna.bridges.create_account_alias",
    "fauna.bridges.create_account_list",
    "fauna.bridges.delete_account_alias",
    "fauna.bridges.delete_account_list",
    "fauna.bridges.delete_addressbook",
    "fauna.bridges.delete_card",
    "fauna.bridges.delete_event",
    "fauna.bridges.discard_export_blob",
    "fauna.bridges.enable_account_alias",
    "fauna.bridges.fail_export_session",
    "fauna.bridges.fail_import_session",
    "fauna.bridges.feeds.create",
    "fauna.bridges.feeds.delete",
    "fauna.bridges.feeds.list",
    "fauna.bridges.fetch_bridge_pubkey",
    "fauna.bridges.fetch_export_chunk_ciphertext",
    "fauna.bridges.fetch_spam_model",
    "fauna.bridges.finalize_export_session",
    "fauna.bridges.finalize_import_session",
    "fauna.bridges.generate_disposable_alias",
    "fauna.bridges.get_alias_policy",
    "fauna.bridges.get_caldav_port",
    "fauna.bridges.get_forward_all_to",
    "fauna.bridges.get_mail_config",
    "fauna.bridges.get_mail_serving_enabled",
    "fauna.bridges.get_primary_domain_rename_status",
    # An Admin-class read (R2): the spam baseline's state mutates nothing.
    "fauna.bridges.get_spam_baseline_state",
    "fauna.bridges.get_spam_scoring_policy",
    # Both threshold-override doors key on the CALLER's actor id
    # (`bridge_routing_handlers.rs::{get,set}_spam_threshold_override_handler`
    # → `db.{get,set}_spam_threshold_override(&actor_id, …)`), so they sit on
    # the account side beside the scoring policy — unlike `put_spam_policy` /
    # `publish_spam_baseline`, which are nest-wide.
    "fauna.bridges.get_spam_threshold_override",
    "fauna.bridges.set_spam_threshold_override",
    # The hourly forward cap: the same caller-keyed per-account doors
    # (`{get,set}_forward_per_hour_handler` → `db.{get,set}_forward_per_hour`).
    "fauna.bridges.get_forward_per_hour",
    "fauna.bridges.set_forward_per_hour",
    "fauna.bridges.import_account_aliases",
    "fauna.bridges.import_message",
    "fauna.bridges.import_message_batch",
    "fauna.bridges.link",
    "fauna.bridges.link_challenge",
    "fauna.bridges.list",
    "fauna.bridges.list_account_alias_hits",
    "fauna.bridges.list_account_aliases",
    "fauna.bridges.list_account_lists",
    "fauna.bridges.list_addressbooks",
    "fauna.bridges.list_blocklist_self_check_history",
    "fauna.bridges.list_calendars",
    "fauna.bridges.list_deliverability_diagnostic_runs",
    "fauna.bridges.list_dkim_selectors",
    "fauna.bridges.list_export_sessions",
    "fauna.bridges.list_follow_requests",
    "fauna.bridges.list_follows",
    "fauna.bridges.list_forwarders",
    "fauna.bridges.list_import_sessions",
    "fauna.bridges.list_list_members",
    "fauna.bridges.list_list_send_history",
    "fauna.bridges.list_local_domains",
    "fauna.bridges.list_own_mailboxes",
    "fauna.bridges.list_pending_bridges",
    "fauna.bridges.list_primary_domain_renames",
    "fauna.bridges.list_service_users",
    "fauna.bridges.list_spam_training_history",
    "fauna.bridges.mail_health",
    "fauna.bridges.outbound_warmup_status",
    "fauna.bridges.pause_export_session",
    "fauna.bridges.pause_import_session",
    "fauna.bridges.provision_addressbook",
    "fauna.bridges.provision_calendar",
    "fauna.bridges.provision_mls_snapshot_blob",
    "fauna.bridges.provision_recipient_mls_pubkey",
    "fauna.bridges.provision_webdav_keys_blob",
    "fauna.bridges.provision_wrapped_mls_blob",
    "fauna.bridges.provision_wrapped_submission_token",
    "fauna.bridges.put_card_ciphertext",
    "fauna.bridges.put_event_ciphertext",
    "fauna.bridges.put_spam_model",
    "fauna.bridges.query_cards",
    "fauna.bridges.query_events",
    "fauna.bridges.remove_follow",
    "fauna.bridges.reset_spam_model",
    "fauna.bridges.resolve_follow_request",
    "fauna.bridges.restart_export_session",
    "fauna.bridges.resubscribe_list_member",
    "fauna.bridges.resume_export_session",
    "fauna.bridges.resume_import_session",
    "fauna.bridges.revoke_account_alias",
    "fauna.bridges.revoke_wrapped_mls_blob",
    "fauna.bridges.revoke_wrapped_submission_token",
    "fauna.bridges.send_list_message",
    "fauna.bridges.set_baseline_contribution",
    "fauna.bridges.set_forward_all_to",
    "fauna.bridges.set_mail_serving_enabled",
    "fauna.bridges.set_settings",
    "fauna.bridges.start_export_session",
    "fauna.bridges.start_import_session",
    "fauna.bridges.sync_addressbook_since",
    "fauna.bridges.sync_calendar_since",
    "fauna.bridges.unlink",
    "fauna.bridges.unsubscribe_list_member",
    "fauna.bridges.update_account_alias",
    "fauna.bridges.update_account_list",
    "fauna.bridges.upload_export_chunk",
})

#: The complement — every registered admin/bridge kind that may touch state
#: beyond the caller's own account. Stored explicitly rather than computed so
#: the equality pin has something to be equal *to*; `classify` never consults it
#: (it tests non-membership of the account-scoped set), which is what keeps an
#: unclassified kind global by default.
GLOBAL_ADMIN_KINDS = frozenset({
    "fauna.admin.admins.add",
    "fauna.admin.admins.remove",
    "fauna.admin.deployment_seed.rotate",
    "fauna.admin.factory_reset",
    "fauna.admin.gc",
    "fauna.admin.invite_codes.create",
    "fauna.admin.invite_codes.delete",
    "fauna.admin.invite_requests.approve",
    "fauna.admin.invite_requests.deny",
    "fauna.admin.membership_tiers.clear",
    "fauna.admin.membership_tiers.set",
    "fauna.admin.region.set",
    "fauna.admin.request_host_restart",
    "fauna.admin.services.update",
    # A deployment-wide policy toggle, not an account setting: the handler
    # takes no actor scope beyond the permission check
    # (`node_policy_handlers.rs::set_age_verification_required_handler` →
    # `node_policy_core::apply_age_verification_required_change(&state, …)`),
    # so it belongs with the other nest-wide `set_*` policy doors.
    "fauna.admin.set_age_verification_required",
    "fauna.admin.set_cors_origins",
    "fauna.admin.set_max_storage_bytes",
    "fauna.admin.set_registration_mode",
    "fauna.admin.set_serving_port",
    "fauna.admin.set_subhandles",
    "fauna.admin.tiers.create",
    "fauna.admin.tiers.update",
    "fauna.admin.users.cancel_eviction",
    "fauna.admin.users.clear_handle",
    "fauna.admin.users.evict",
    "fauna.admin.web_app_origin.set",
    "fauna.bridges.abort_primary_domain_rename",
    "fauna.bridges.add_local_domain",
    "fauna.bridges.append",
    "fauna.bridges.approve_pending_bridge",
    "fauna.bridges.atproto.deliver_permission_set",
    "fauna.bridges.atproto.end_session",
    "fauna.bridges.atproto.fetch_app_credential_verifiers",
    "fauna.bridges.atproto.fetch_identities",
    "fauna.bridges.atproto.fetch_identity_key_blob",
    "fauna.bridges.atproto.fetch_issuer_jwks",
    "fauna.bridges.atproto.fetch_preferences",
    "fauna.bridges.atproto.fetch_profile",
    "fauna.bridges.atproto.fetch_public_posts",
    "fauna.bridges.atproto.fetch_session_secret_blob",
    "fauna.bridges.atproto.ingest_external_write",
    "fauna.bridges.atproto.record_blob",
    "fauna.bridges.atproto.record_minted_identity",
    "fauna.bridges.atproto.record_session",
    "fauna.bridges.atproto.refresh_session",
    "fauna.bridges.atproto.store_preferences",
    "fauna.bridges.blocklist_self_check_run",
    "fauna.bridges.check_greylist",
    "fauna.bridges.check_submission_quota",
    "fauna.bridges.complete_primary_domain_rename",
    "fauna.bridges.copy",
    "fauna.bridges.create_forwarder",
    "fauna.bridges.create_mailbox",
    "fauna.bridges.decode_srs_bounce",
    "fauna.bridges.delete_forwarder",
    "fauna.bridges.delete_mailbox",
    "fauna.bridges.deliver_sealed_scheduling",
    "fauna.bridges.enqueue_outbound_mail",
    "fauna.bridges.expunge",
    "fauna.bridges.extend_primary_domain_rename_grace",
    "fauna.bridges.fetch_config",
    "fauna.bridges.fetch_index_segments_since",
    "fauna.bridges.fetch_message_ciphertext",
    "fauna.bridges.fetch_message_metadata",
    "fauna.bridges.fetch_mls_snapshot_blob",
    "fauna.bridges.fetch_mta_sts_policy",
    "fauna.bridges.fetch_outbound_due",
    "fauna.bridges.fetch_recipient_filters",
    "fauna.bridges.fetch_recipient_forward_config",
    "fauna.bridges.fetch_recipient_index_key",
    "fauna.bridges.fetch_recipient_mls_pubkey",
    "fauna.bridges.fetch_tls_cert_blob",
    "fauna.bridges.fetch_tlsa",
    "fauna.bridges.fetch_webdav_keys_blob",
    "fauna.bridges.fetch_wrapped_mls_blob",
    "fauna.bridges.fetch_wrapped_submission_token",
    "fauna.bridges.force_rotate_dkim",
    "fauna.bridges.forward_message",
    "fauna.bridges.get_quota",
    "fauna.bridges.ingest_inbound_mail",
    "fauna.bridges.list_mailboxes",
    "fauna.bridges.list_messages",
    "fauna.bridges.mark_outbound_bounced",
    "fauna.bridges.mark_outbound_delivered",
    "fauna.bridges.mark_outbound_failed",
    "fauna.bridges.mint_bulk_byte_token",
    "fauna.bridges.move",
    "fauna.bridges.outbound_warmup_reset",
    "fauna.bridges.place_inbound_invite",
    "fauna.bridges.provision_self_signed_cert",
    "fauna.bridges.provision_tls_cert_blob",
    "fauna.bridges.publish_spam_baseline",
    "fauna.bridges.put_alias_policy",
    "fauna.bridges.put_auth_policy",
    "fauna.bridges.put_imap_policy",
    "fauna.bridges.put_outbound_policy",
    "fauna.bridges.put_spam_policy",
    "fauna.bridges.put_submission_policy",
    "fauna.bridges.register_service_user",
    "fauna.bridges.reject_pending_bridge",
    "fauna.bridges.remove_local_domain",
    "fauna.bridges.rename_mailbox",
    "fauna.bridges.report_auth_event",
    "fauna.bridges.report_log_events",
    "fauna.bridges.report_rejected_scan",
    "fauna.bridges.report_session_close",
    "fauna.bridges.report_tls_attempt",
    "fauna.bridges.request_enrollment",
    "fauna.bridges.resolve_mx",
    "fauna.bridges.resolve_recipient",
    "fauna.bridges.restore_local_domain",
    "fauna.bridges.restore_real_tls_cert",
    "fauna.bridges.revoke_dkim_blob",
    "fauna.bridges.revoke_service_user",
    "fauna.bridges.rotate_list_unsubscribe_secret",
    "fauna.bridges.rotate_srs_secret",
    "fauna.bridges.run_deliverability_diagnostics",
    "fauna.bridges.search_messages",
    "fauna.bridges.select_mailbox",
    "fauna.bridges.send_auto_reply",
    "fauna.bridges.set_auto_enable_mail_for_new_users",
    "fauna.bridges.set_caldav_enabled",
    "fauna.bridges.set_caldav_port",
    "fauna.bridges.set_carddav_enabled",
    "fauna.bridges.set_catch_all_actor",
    "fauna.bridges.set_dkim_rotation_days",
    "fauna.bridges.set_mail_enabled",
    "fauna.bridges.set_role_address",
    "fauna.bridges.set_webdav_enabled",
    "fauna.bridges.start_primary_domain_rename",
    "fauna.bridges.store_flags",
    "fauna.bridges.submit_inbound_mail",
    "fauna.bridges.subscribe_mailbox",
    "fauna.bridges.subscribe_mailbox_state",
    "fauna.bridges.unsubscribe_mailbox",
    "fauna.bridges.update_local_domain_config",
    "fauna.bridges.validate_recipient",
    "fauna.bridges.webdav_admit_principal",
    "fauna.bridges.webdav_list_folders",
    "fauna.bridges.webdav_list_files",
    "fauna.bridges.webdav_quota",
    "fauna.bridges.webdav_record_change",
    "fauna.bridges.whoami",
})

#: The closed vocabulary itself — what `kind.rs` registers, and the set the body
#: scan recognizes a string literal against.
KIND_VOCABULARY = ACCOUNT_SCOPED_KINDS | GLOBAL_ADMIN_KINDS

#: Read-*shaped* kinds whose handlers write. They are already global above (they
#: are bridge-role-class, so the R1/R2 rules never admitted them), and this set
#: adds nothing to `classify` — it exists so the next session to tidy the
#: partition by name has to argue with a test instead of with a comment. Each
#: was confirmed at its handler, not from documentation: `kind.rs`'s own comment
#: on this cluster names `fetch_index_segments_since`, which does **not** seed,
#: and omits `list_mailboxes`, `fetch_message_ciphertext` and `check_greylist`,
#: which do.
WRITES_ON_READ_KINDS = frozenset({
    "fauna.bridges.check_greylist",
    "fauna.bridges.check_submission_quota",
    "fauna.bridges.fetch_message_ciphertext",
    "fauna.bridges.fetch_message_metadata",
    "fauna.bridges.fetch_outbound_due",
    "fauna.bridges.fetch_tls_cert_blob",
    "fauna.bridges.list_mailboxes",
    "fauna.bridges.list_messages",
    "fauna.bridges.select_mailbox",
})

#: The marker a test carries when its OWN BODY mutates global admin state. It
#: predates the body scan below and is now a *declaration* rather than the only
#: inference: a test whose reach the call graph cannot see (a kind assembled at
#: runtime, an f-string) still has this to fall back on.
GLOBAL_ADMIN_MARKER = "global_admin"

#: Class (3)'s individual re-admission, under § The shared-box rule's
#: non-destructive carve-out: a test that mutates global state and meets that
#: standard — a teardown that *proves* it restores what it changed in the
#: ordinary case; an additive-only act the box's admin opts into by env in the
#: two live-only residents — may carry this to run on live anyway, with the
#: carve-out's blast-radius argument in its docstring. It overrides
#: `GLOBAL_ADMIN_MARKER`, the fixture-closure inference and the admin-shell
#: scan alike — never the local-binary classes, which are structural rather
#: than a policy.
LIVE_READMIT_MARKER = "live_ok"

_GLOBAL_ADMIN_REASON = (
    "mutates nest state outside the run's own account ({why}), which the "
    "shared-box rule forbids against a live box a human may be signed into "
    "(testing.md § Default app and nest mode — exclusion class (3) "
    "global-admin-mutating; § The shared-box rule). A test that meets the "
    "shared-box rule's non-destructive carve-out may re-admit itself with "
    "@pytest.mark." + LIVE_READMIT_MARKER + "; a run against a staging box nobody "
    "is signed into declares it with --live-box " + nest_mode_mod.BOX_DISPOSABLE
)

#: ── The app door: the admin shell's element IDs ──────────────────────────
#:
#: The two kind halves above see a mutation Python SENDS. They cannot see one
#: the APP sends: a journey test drives the admin shell through the UI
#: (e2e-conventions point 8 — a journey's mutations go through the app, never
#: an API call standing in for the user), so the kind the nest executes leaves
#: the tui/app process, and no string in the Python call graph names it.
#: Measured 2026-10-04 on a tui sweep against the staging box: the OAuth issuer
#: key and session secret rotated, the web apex actor designated, a spam
#: penalty set to 1000, NAT mode, pairing and the DAV toggles flipped — every
#: one by a test the kind scan read as eligible.
#:
#: The signal is the same shape the kinds gave: a **closed vocabulary** the
#: spec registers. ui.yaml owns every element ID (e2e-conventions point 1), and
#: lists each admin page's elements under a page named `admin-*`; a test that
#: names one of those IDs — itself, or through an action method, or through a
#: fixture in its closure — has the app on an admin page. **There is no
#: account-scoped half to admit**: the admin shell is, by definition, the one
#: surface where every admin-chosen, nest-wide value is set (principles.md §
#: One configuration surface), so where the kinds needed R1–R3 to keep reads in,
#: the shell needs nothing — a read-only admin journey lost to live is the safe
#: direction, exactly as an unclassified admin kind is. An ID the spec lists on
#: an admin page AND on a non-admin page (`error-message`, `page-heading`, the
#: log viewer's rows) says nothing about where the app is and is left out.
#:
#: Derived from ui.yaml at the first live classification and never hand-listed,
#: for the reason `ui_walk.canonical_pages` gives: a hand-written list drifts
#: the day someone adds a page, and drifts downward, the one direction this
#: mechanism must never fail in. The pin in `test_nest_mode_axis.py` checks the
#: derivation against known members and known non-members, not a copy.
_UI_YAML = pathlib.Path(__file__).resolve().parent.parent / "ui.yaml"

#: A ui.yaml page whose name starts with this is a page of the admin shell.
ADMIN_SHELL_PAGE_PREFIX = "admin-"

#: Admin-shell IDs a test may name without driving the shell: the shell's own
#: way OUT. `common/launch_harness.SHELL_SWAP_ANCHORS` waits on `admin-nav-back`
#: to learn that a relaunch came back into an authenticated shell, and pressing
#: it only navigates — so it marks no journey as admin-mutating, and a journey
#: that does drive the shell names that shell's other IDs on the way in.
#: Measured 2026-10-05: once `kind_reach` resolved module constants, the anchor
#: tuple put this one ID in every relaunching journey's reach and pulled a dozen
#: tui journeys off the live box for it.
ADMIN_SHELL_EXIT_IDS = frozenset({"admin-nav-back"})


def _spec_ids(entries) -> set[str]:
    """The IDs in a ui.yaml `elements:` / `components:` list.

    Entries are bare strings; a mapping form (an ID with per-platform flags)
    contributes its one key. Anything else is not an ID and is skipped.
    """
    out: set[str] = set()
    for entry in entries or ():
        if isinstance(entry, str):
            out.add(entry)
        elif isinstance(entry, dict) and len(entry) == 1:
            out.add(next(iter(entry)))
    return out


@functools.lru_cache(maxsize=1)
def admin_shell_element_ids() -> frozenset[str]:
    """Every element ID ui.yaml lists on an `admin-*` page and on no other page.

    A page's IDs are its own `elements:` plus the `elements:` of each component
    it declares (`admin-stat-card` → its label and value). Read once per run,
    and only when a live classification asks — the standalone inner loop pays
    nothing, the nest-mode axis's standing requirement.
    """
    import yaml  # lazy: only a live run loads the spec

    with open(_UI_YAML, encoding="utf-8") as f:
        spec = yaml.safe_load(f)
    pages = spec.get("pages") or {}
    components = spec.get("components") or {}
    admin: set[str] = set()
    other: set[str] = set()
    for name, page in pages.items():
        ids = _spec_ids((page or {}).get("elements"))
        for component in _spec_ids((page or {}).get("components")):
            ids.add(component)
            ids |= _spec_ids((components.get(component) or {}).get("elements"))
        (admin if name.startswith(ADMIN_SHELL_PAGE_PREFIX) else other).update(ids)
    if not admin:
        raise AssertionError(
            f"{_UI_YAML} lists no `pages.{ADMIN_SHELL_PAGE_PREFIX}*` page — the "
            "admin-shell scan reads its vocabulary from the spec, so an empty "
            "set would silently admit every admin journey to the live box. Fix "
            "the loader or the spec; do not hand-list the IDs here."
        )
    return frozenset(admin - other)


# ── The live-mode harness gaps (2026-10-05) ─────
# A full tui sweep against dev.example.com recorded tests as failed live cells
# that live mode could never have passed: each needed something only a nest
# this harness started can give. Every class below is decided at collection,
# from the test's own code or its fixtures, so the run deselects instead of
# recording — testing.md § Default app and nest mode → *Live mode*.

#: The tier marker of an in-process test (testing.md § The four-tier
#: taxonomy). A nest mode is a harness input meaningful only within tiers 3-4,
#: so a tier_1 test in a docker or live run asserts nothing about the mode and
#: some assert the standalone environment itself (the ledger's off-live pins,
#: the r14 trust-seed default) or contend with the run's own slot (the build
#: slot pins).
IN_PROCESS_TIER_MARKER = "tier_1"

_IN_PROCESS_TIER_REASON = (
    "is tier_1 — in-process, with no nest for the {mode!r} mode to choose "
    "(testing.md § Default app and nest mode: a mode is meaningful only within "
    "tiers 3-4). It runs in the standalone inner loop, where it asserts the "
    "environment it was written for"
)

_ABSENT_CAPABILITY_REASON = (
    "reads {keys} off the nest handle, and nest mode {mode!r} answers none of "
    "the capability keys: the nest is on another machine, so there is no local "
    "process to stop, database to open or data dir to read (nest_mode.LIVE_ABSENT "
    "— testing.md § Default app and nest mode, (a)). Found in the test's own "
    "code, at collection, instead of as a NestCapabilityError recorded as a "
    "failed cell"
)

#: The entry points a test's own code starts a nest through. A provider's
#: `start`, and the two conftest doors every fixture routes through; a truthy
#: start-option keyword passed to one of them is a request the provider must
#: honour (`_OptionAwareProvider._refuse_unsupported`).
NEST_START_ENTRY_POINTS = frozenset({"start", "_start_dedicated_nest", "_make_nest"})

#: Every start option any fixture asks for — the vocabulary
#: `FIXTURE_START_OPTIONS` already pins to the tree.
START_OPTION_VOCABULARY = frozenset().union(*FIXTURE_START_OPTIONS.values())

#: The label the start-option reasons give a test's own nest start, where the
#: fixture verdict names fixtures.
_OWN_START = "the test's own code"

_ONE_LIVE_BOX_REASON = (
    "needs {count} distinct nests ({fixtures}), and every nest a live run starts "
    "is the same box (testing.md § Default app and nest mode → Live mode, "
    "(c)): a second user, a backup destination or a linked nest lands on the "
    "box the first one is, where the identity already exists"
)

#: Fixtures whose premise only a nest this harness started holds, beside the
#: marker (`HARNESS_BOX_MARKER`) a test body declares the same premise with.
#: Live only — a docker nest is a fresh nest on this machine's loopback too.
HARNESS_BOX_FIXTURES = frozenset({
    # A domainless (localhost) nest, on which the hosted AT Protocol rungs grey
    # out; a real public box has a domain, so they are enabled there.
    "atproto_localhost_nest",
    # A fresh nest whose feed holds only the import: the imported records'
    # original dates sort below a populated box's first page.
    "archive_nest",
})

HARNESS_BOX_MARKER = "harness_box"

_HARNESS_BOX_REASON = (
    "asserts a premise only a nest this harness started holds ({why}) — a "
    "loopback address, no public domain, a feed nobody else posts to — which a "
    "deployed box does not (testing.md § Default app and nest mode → Live mode)"
)


def _marker_names(item) -> set[str]:
    return {m.name for m in item.iter_markers()}


def _declared_options(mode: nest_mode_mod.NestMode) -> frozenset[str] | None:
    """This mode's provider's `supported_options`, or None if none is registered.

    Unregistered is not a state a real run reaches — conftest registers all three
    providers at import and `pytest_configure` refuses an unbuilt mode outright —
    but a *classifier* must not be the thing that raises about it, so the rule
    stays silent instead of guessing a set. Guessing is wrong in both directions:
    an empty set would exclude every option-passing fixture from a mode nobody
    asked about, and a full one would silently disable the class.
    """
    try:
        return nest_mode_mod.supported_options(mode)
    except nest_mode_mod.NestModeError:
        return None


#: `tests/e2e-unified` — the root the mail-venue reader map's keys are relative
#: to. Derived from this module's own location rather than from pytest's rootdir,
#: which moves with the invocation (repo root for a `just` recipe, the suite dir
#: for a bare `pytest`) and would silently make every key miss.
_E2E_ROOT = pathlib.Path(__file__).resolve().parent.parent


def _venue_reader_node(item) -> str:
    """`<path relative to tests/e2e-unified>::<test function>` for `item`.

    The key shape `MAIL_VENUE_HOST_AFFORDANCE_READERS` uses, built the same way
    the AST pin that re-derives that map builds it — so the two cannot disagree
    about what a node is called. Returns `""` for an item with no readable path,
    which no real collected test has and which simply never matches.
    """
    path = getattr(item, "path", None)
    if path is None:
        return ""
    try:
        rel = pathlib.Path(path).resolve().relative_to(_E2E_ROOT).as_posix()
    except ValueError:
        return ""
    return f"{rel}::{kind_reach.test_function_name(item)}"


def _declared_venue_options(mode: nest_mode_mod.NestMode) -> frozenset[str] | None:
    """This mode's provider's `supported_venue_options`, or None if unregistered.

    The venue twin of `_declared_options`, silent on an unregistered provider for
    exactly the same reason: a classifier must not be the thing that raises.
    """
    try:
        return nest_mode_mod.supported_venue_options(mode)
    except nest_mode_mod.NestModeError:
        return None


def _first_three(kinds) -> str:
    """A reason-string rendering of a kind set: the first three, then a count."""
    named = ", ".join(sorted(kinds)[:3])
    if len(kinds) > 3:
        named += f", +{len(kinds) - 3} more"
    return named


def _verdicts(item, mode: nest_mode_mod.NestMode):
    """Every ratified rule that applies to `item` in `mode`, in ratified order.

    A **lazy** sequence, and both public entry points are one line over it:
    `classify` takes the first (the run wants one exclusion and one reason),
    `all_rules` takes them all (the audit wants the whole account). Generator
    semantics keep the laziness `classify` always had — the `kind_reach` call
    graph is still only built if no earlier rule matched — while leaving exactly
    one definition of each rule, so the two answers cannot drift.

    Why the audit needs more than the first: the ratified order puts
    `nest_binary` (MIXED — closable where the binary is incidental) ahead of
    `bridge_spawn` (a FACT), so a fixture doing BOTH reports the closable class.
    `dedicated_mail_nest` is exactly that shape — it requests `nest_binary` and
    spawns MTA+MDA on the host — and it reached 3 of the 5 pages a measured
    docker run called `nest_binary`-sole-blocked. Reading the first match as the
    whole story systematically overstates how much is closable.

    That overstatement has now arrived three times, the third (2026-09-02)
    through a door this docstring did not consider: not a fixture doing both,
    but a test BODY doing the bridge half with no bridge fixture at all — see
    `BRIDGE_BINARY_FIXTURES`. When the account looks tidy, suspect a door.
    """
    markers = _marker_names(item)

    # An explicit `@pytest.mark.<mode>_only` is a declared exclusion from the
    # other two modes. The marker names come from nest_mode so the error message
    # a NestCapabilityError prints ("mark the test @pytest.mark.docker_only")
    # names a marker this function actually honours.
    for other, marker in nest_mode_mod.MODE_MARKERS.items():
        if marker in markers and other != mode.name:
            yield Verdict(
                DECLARED_ABSENCE,
                f"marked @pytest.mark.{marker}: declared to run only in "
                f"{other!r} mode",
                RULE_MODE_MARKER,
            )

    if mode.is_standalone:
        # Standalone can host everything the harness can build — it is the mode
        # every fixture was written against.
        return

    # A tier_1 test has no nest for a mode to choose (testing.md: a mode is
    # meaningful only within tiers 3-4), so a docker or live run carries it
    # only as weight — and as reds, where it pins the standalone environment.
    if IN_PROCESS_TIER_MARKER in markers:
        yield Verdict(
            DECLARED_ABSENCE,
            _IN_PROCESS_TIER_REASON.format(mode=mode.name),
            RULE_IN_PROCESS_TIER,
        )
        return

    # Real fixture requests only. Every intersection below turns a name into a
    # DECLARED_ABSENCE — a silent deselect — so a directly-parametrized argname
    # that merely shares a fixture's name would drop a test from a docker/live
    # run with a reason that reads perfectly plausible. None of these names
    # collides today (they are long and specific), which is exactly why the
    # same bug went unnoticed for months where the name was `app`; see
    # `helpers/fixture_closure.py`.
    closure = real_fixture_closure(item)

    binaries = NEST_BINARY_FIXTURES.intersection(closure)

    # Class (9) is a REFINEMENT of the binary closure, not a second verdict on
    # top of it: a test whose only nest build is `ap_binary` gets the sharp
    # reason (a compiled-in `test-hooks` the artifact can never carry, a FACT)
    # instead of the blunt one (`nest_binary`, MIXED and read as closable
    # routing work). So the members are SUBTRACTED below rather than merely
    # reported alongside — leaving them in both buckets would keep the account
    # this class exists to correct.
    #
    # They stay in `NEST_BINARY_FIXTURES` all the same: that set answers "does
    # this fixture compile a nest", it is AST-derived from exactly that
    # question, and the answer is yes. A test that requests both an in-class
    # binary and an ordinary one is genuinely blocked twice and says so.
    feature_set = set(FEATURE_SET_BINARY_FIXTURES).intersection(closure)
    if feature_set:
        named = sorted(feature_set)
        yield Verdict(
            DECLARED_ABSENCE,
            _FEATURE_SET_BINARY_REASON.format(
                fixtures=", ".join(named),
                features=FEATURE_SET_BINARY_FIXTURES[named[0]],
                mode=mode.name,
            ),
            RULE_FEATURE_SET_BINARY,
        )

    incidental = binaries - feature_set
    if incidental:
        yield Verdict(
            DECLARED_ABSENCE,
            _NEST_BINARY_REASON.format(
                fixtures=", ".join(sorted(incidental)), mode=mode.name
            ),
            RULE_NEST_BINARY,
        )

    local = LOCAL_NEST_FIXTURES.intersection(closure)
    if local:
        yield Verdict(
            DECLARED_ABSENCE,
            _LOCAL_FIXTURE_REASON.format(
                fixtures=", ".join(sorted(local)),
                verb="spawns" if len(local) == 1 else "spawn",
            ),
            RULE_LOCAL_NEST_FIXTURE,
        )

    # Class (4), ruling (3)'s seam: the fixture wants a knob this mode's
    # provider does not declare. Sits directly after the two rules it is the
    # SUCCESSOR to — while every one of these fixtures still requests
    # `nest_binary`, that closure answers first and this never reports; once arm
    # 1 routes them through providers, this is the reason that remains, and it
    # is a narrower and truer one (the fixture is not excluded because a binary
    # exists, but because one specific knob is unturnable here).
    #
    # Read off the PROVIDER's own declaration, never a second list: a provider
    # that grows an option un-excludes every fixture needing only what it now
    # supports, with no edit here.
    #
    # Two doors the closure cannot see join it here (2026-10-05): a fixture the
    # test's own code requests LAZILY (`request.getfixturevalue(
    # "self_signed_nest")`), and a start option its own code passes straight
    # to a provider (`provider.start(..., unclaimed=True)`). Both used to reach
    # the provider's backstop at setup, which a live run recorded as a failed
    # cell. Only this rule reads the lazy names: a lazy request is often
    # mode-conditional (`nest_binary` behind `builds_local_nest`), and a
    # fixture the run never requests must not exclude through the binary rules.
    lazy = (
        kind_reach.lazy_fixtures_requested_by_own_code(item)
        & FIXTURE_START_OPTIONS.keys()
    )
    wants = FIXTURE_START_OPTIONS.keys() & (closure | lazy)
    inline = kind_reach.start_options_passed_by_own_code(
        item, START_OPTION_VOCABULARY, NEST_START_ENTRY_POINTS
    )
    if wants or inline:
        supported = _declared_options(mode)
        needed = frozenset(inline).union(
            *(FIXTURE_START_OPTIONS[f] for f in wants)
        )
        sources = sorted(wants) + ([_OWN_START] if inline else [])
        unsupported = needed - supported if supported is not None else frozenset()

        # The split ruling (3) always implied and the audit could not read: an
        # option this mode does not honour is either a knob a provider could
        # GROW — closable debt, and the reason this class is graded MIXED — or
        # one it can never turn, which makes the blanked cell honestly blank
        # forever. Both were saying the same sentence.
        #
        # Not cosmetic. Docker honours every option a fixture asks for EXCEPT
        # `handle_domain_seed`, whose absence ruling (3) settles by name, so
        # before this split the class was 100% permanent while reading as 100%
        # closable — and it was the last MIXED sole-blocker left in the audit
        # (`conversations`). Live is the same fact in the other shape: it
        # started no nest, so no option is honourable there, ever.
        #
        # Asked of the PROVIDER, like `supported_options` itself, so this stays
        # one source of truth rather than a second list to keep in step.
        permanent = (
            nest_mode_mod.permanently_unsupported(mode, unsupported)
            if unsupported else frozenset()
        )
        for options, rule, reason in (
            (permanent, RULE_PERMANENT_OPTION, _PERMANENT_OPTION_REASON),
            (unsupported - permanent, RULE_UNSUPPORTED_OPTION,
             _UNSUPPORTED_OPTION_REASON),
        ):
            if not options:
                continue
            yield Verdict(
                DECLARED_ABSENCE,
                reason.format(
                    fixtures=", ".join(sources),
                    verb="starts" if len(sources) == 1 else "start",
                    options=", ".join(sorted(options)),
                    mode=mode.name,
                    have=(
                        "it honours no per-nest start options"
                        if not supported
                        else f"it honours only {', '.join(sorted(supported))}"
                    ),
                ),
                rule,
            )

    # Ruling (3)'s VENUE seam, sitting directly after its start-option twin
    # because it is the same question one layer up: not "can this mode turn that
    # knob" but "can this mode stand up that shape of mail venue at all".
    venue_wants = MAIL_VENUE_FIXTURE_OPTIONS.keys() & closure
    if venue_wants:
        supported = _declared_venue_options(mode)
        needed = frozenset().union(
            *(MAIL_VENUE_FIXTURE_OPTIONS[f] for f in venue_wants))
        unsupported = needed - supported if supported is not None else frozenset()
        if unsupported:
            yield Verdict(
                DECLARED_ABSENCE,
                _UNSUPPORTED_VENUE_REASON.format(
                    fixtures=", ".join(sorted(venue_wants)),
                    verb="wants" if len(venue_wants) == 1 else "want",
                    options=", ".join(sorted(unsupported)),
                    mode=mode.name,
                    have=(
                        "it stands up no mail venue at all"
                        if not supported
                        else f"it honours only {', '.join(sorted(supported))}"
                    ),
                ),
                RULE_VENUE_OPTION,
            )

    # The venue's BODY half: a test that reads a host-spawn affordance off the
    # venue handle. The fixture routes; this one test cannot, because the thing
    # it reads is a harness-held process or a host path that a supervised
    # container simply does not have. An exact map with a disposition each, so a
    # new reader is an edit someone has to justify.
    if venue_wants:
        node = _venue_reader_node(item)
        if node in MAIL_VENUE_HOST_AFFORDANCE_READERS:
            yield Verdict(
                DECLARED_ABSENCE,
                _VENUE_HOST_AFFORDANCE_REASON.format(
                    disposition=MAIL_VENUE_HOST_AFFORDANCE_READERS[node],
                    mode=mode.name,
                ),
                RULE_VENUE_HOST_AFFORDANCE,
            )

    # The image twin of the binary rule above, checked in the same place and for
    # the same reason: a test that brings its own nest artifact is a witness of
    # THAT artifact, never of the run's.
    own_image = OWN_IMAGE_FIXTURES.intersection(closure)
    if own_image:
        yield Verdict(
            DECLARED_ABSENCE,
            _OWN_IMAGE_REASON.format(
                fixtures=", ".join(sorted(own_image)),
                verb="boots" if len(own_image) == 1 else "boot",
                mode=mode.name,
            ),
            RULE_OWN_IMAGE,
        )

    # Checked ahead of the live-only class below, so both non-standalone modes
    # give the same — and the more fundamental — reason. Several of these
    # fixtures are ALSO global-admin-mutating, and live used to refuse them on
    # that ground alone; the shared-box rule is a policy someone could relax,
    # whereas the loopback gate is the product refusing to enroll them at all.
    #
    # Both doors, ONE verdict. `disposable_mta_bridge` requests a spawning
    # fixture AND the binary, and two evidence sources for one fact must not
    # become two entries: the audit tallies rule ids, so a page counted twice
    # under `bridge_spawn` would overstate the class as surely as missing it
    # understates it.
    bridges = BRIDGE_SPAWN_FIXTURES.intersection(closure)
    bridge_bins = BRIDGE_BINARY_FIXTURES.intersection(closure)
    if bridges or bridge_bins:
        clauses = []
        if bridges:
            clauses.append(_BRIDGE_SPAWN_CLAUSE.format(
                fixtures=", ".join(sorted(bridges)),
                verb="spawns" if len(bridges) == 1 else "spawn",
            ))
        if bridge_bins:
            clauses.append(_BRIDGE_BINARY_CLAUSE.format(
                fixtures=", ".join(sorted(bridge_bins)),
            ))
        yield Verdict(
            DECLARED_ABSENCE,
            _BRIDGE_SPAWN_REASON.format(
                evidence=", and ".join(clauses), mode=mode.name
            ),
            RULE_BRIDGE_SPAWN,
        )

    # Class (5)'s body half, for BOTH non-standalone modes: a test that drives
    # the loopback-gated kind itself, with no bridge fixture to give it away.
    # Built on the first non-standalone classification, like class (3)'s, so the
    # standalone inner loop pays nothing for it.
    loopback_kinds = kind_reach.kinds_reached_by(
        kind_reach.test_function_name(item), LOOPBACK_GATED_KINDS
    )
    if loopback_kinds:
        yield Verdict(
            DECLARED_ABSENCE,
            _LOOPBACK_KIND_REASON.format(
                kinds=", ".join(sorted(loopback_kinds)), mode=mode.name
            ),
            RULE_LOOPBACK_KIND,
        )

    # Class (8), DOCKER ONLY: nest A dialing nest B across the run's
    # user-defined network is refused as non-global. Live is untouched — a live
    # peer IS globally routable, which is the whole distinction — and standalone
    # returned long before this point.
    #
    # Gated on the closure holding a nest, so the class can never be reported
    # for a test that starts none. The mode-axis pins read `peer_url` to ASSERT
    # on it, headless and in-process; they are the reason the gate is here, and
    # a reason that read "private-range peer" for a test with no nest at all is
    # exactly the false account the mode audit exists to prevent.
    if mode.is_docker and (closure & _NEST_BEARING_FIXTURES):
        peer_keys = kind_reach.keys_read_by(
            kind_reach.test_function_name(item), PEER_AUTHORITY_KEYS
        )
        if peer_keys:
            yield Verdict(
                DECLARED_ABSENCE,
                _PRIVATE_PEER_REASON.format(
                    keys=", ".join(sorted(peer_keys)), mode=mode.name
                ),
                RULE_PRIVATE_PEER,
            )

    # Class (6), for BOTH non-standalone modes: the route simply is not compiled
    # into a release artifact, so the test can only ever 404 there.
    hook_routes = kind_reach.routes_reached_by(
        kind_reach.test_function_name(item), TEST_HOOK_ROUTE_PREFIX
    )
    if hook_routes:
        named = ", ".join(sorted(hook_routes)[:2])
        if len(hook_routes) > 2:
            named += f", +{len(hook_routes) - 2} more"
        yield Verdict(
            DECLARED_ABSENCE,
            _TEST_HOOK_REASON.format(routes=named, mode=mode.name),
            RULE_TEST_HOOKS,
        )
    hook_kinds = kind_reach.kinds_reached_by(
        kind_reach.test_function_name(item), TEST_HOOK_KINDS
    )
    if hook_kinds:
        yield Verdict(
            DECLARED_ABSENCE,
            _TEST_HOOK_KIND_REASON.format(
                kinds=", ".join(sorted(hook_kinds)), mode=mode.name
            ),
            RULE_TEST_HOOKS,
        )

    # The live-only STRUCTURAL classes: facts about where a live nest is, so
    # neither `live_ok` nor a disposable-box declaration (both about the
    # shared-box POLICY, below) moves them.
    if mode.is_live:
        absent = nest_mode_mod.absent_capabilities(mode)
        # Only where the mode answers NO capability key. Docker answers some,
        # and its shared helpers branch on which (`start_nest_in_place` asks
        # the provider before it reads `node_binary`), so a static read there
        # says nothing about what the run reaches.
        if absent == nest_mode_mod.CAPABILITY_KEYS:
            keys = kind_reach.capability_keys_read_by_own_code(item, absent)
            if keys:
                yield Verdict(
                    DECLARED_ABSENCE,
                    _ABSENT_CAPABILITY_REASON.format(
                        keys=", ".join(sorted(keys)), mode=mode.name
                    ),
                    RULE_ABSENT_CAPABILITY,
                )
        nests = FIXTURE_START_OPTIONS.keys() & (closure | lazy)
        if len(nests) > 1:
            yield Verdict(
                DECLARED_ABSENCE,
                _ONE_LIVE_BOX_REASON.format(
                    count=len(nests), fixtures=", ".join(sorted(nests))
                ),
                RULE_ONE_LIVE_BOX,
            )
        premise = sorted(HARNESS_BOX_FIXTURES & (closure | lazy))
        if HARNESS_BOX_MARKER in markers:
            premise.append(f"declared by @pytest.mark.{HARNESS_BOX_MARKER}")
        if premise:
            yield Verdict(
                DECLARED_ABSENCE,
                _HARNESS_BOX_REASON.format(why=", ".join(premise)),
                RULE_HARNESS_BOX,
            )

    if mode.is_live and not mode.disposable_box:
        # Class (3) is live-only: on a throwaway nest, mutating a mail domain or
        # the spam policy costs nothing and is often the point of the test. And
        # it is a POLICY about the box, not a fact about the mode: a run that
        # declared its live box disposable (`--live-box disposable`,
        # `nest_mode.declare_box` — accepted only for a named staging box) has
        # nobody signed into it to protect, so every door below is off for
        # that run. The structural classes above are untouched by it.
        if LIVE_READMIT_MARKER in markers:
            return
        globals_ = GLOBAL_ADMIN_FIXTURES.intersection(closure)
        if globals_:
            yield Verdict(
                DECLARED_ABSENCE,
                _GLOBAL_ADMIN_REASON.format(
                    why=f"via {', '.join(sorted(globals_))}"
                ),
                RULE_GLOBAL_ADMIN,
            )
        if GLOBAL_ADMIN_MARKER in markers:
            yield Verdict(
                DECLARED_ABSENCE,
                _GLOBAL_ADMIN_REASON.format(
                    why=f"declared by @pytest.mark.{GLOBAL_ADMIN_MARKER}"
                ),
                RULE_GLOBAL_ADMIN,
            )
        # The body half. Neither the fixture closure nor a marker sees a test
        # that names a global kind itself, or reaches one through a helper —
        # which was most of them, and was the reason a green live run used to
        # come with a printed disclaimer instead of a guarantee. The call graph
        # is built here, on the first live classification, so the standalone
        # inner loop never pays for it.
        reached = kind_reach.kinds_reached_by(
            kind_reach.test_function_name(item), KIND_VOCABULARY
        )
        body_globals = reached - ACCOUNT_SCOPED_KINDS
        if body_globals:
            yield Verdict(
                DECLARED_ABSENCE,
                _GLOBAL_ADMIN_REASON.format(why=f"reaches {_first_three(body_globals)}"),
                RULE_GLOBAL_ADMIN,
            )
        # The closure half of the same scan. A fixture's global mutation is
        # the test's as much as a helper's is: `GLOBAL_ADMIN_FIXTURES` names
        # root-conftest fixtures only, so a module fixture (`collision_domain`
        # adding a mail domain) or a session fixture reaching a kind through
        # `tests/common/` (`test_user` lifting `free`) ran on live unseen. A
        # fixture that disables its mutation on live is the one carve-out.
        fixture_globals = {}
        for name in sorted(closure - LIVE_SELF_GATED_FIXTURES - GLOBAL_ADMIN_FIXTURES):
            kinds = kind_reach.kinds_reached_by(name, KIND_VOCABULARY) - ACCOUNT_SCOPED_KINDS
            if kinds:
                fixture_globals[name] = kinds
        if fixture_globals:
            names = ", ".join(sorted(fixture_globals))
            kinds = frozenset().union(*fixture_globals.values())
            yield Verdict(
                DECLARED_ABSENCE,
                _GLOBAL_ADMIN_REASON.format(
                    why=f"fixture {names} reaches {_first_three(kinds)}"
                ),
                RULE_GLOBAL_ADMIN,
            )
        # The app door — the same two scans over the admin shell's element IDs
        # (`admin_shell_element_ids`, above): a journey that mutates through the
        # admin UI sends its kind from the app process, where no kind scan can
        # see it, but it must name an admin-page element to get there. Every
        # admin-shell fixture is caught through its own reach (`admin_app`
        # waits for `admin-dashboard-heading`), so there is no fixture list to
        # keep. A self-gated fixture's reach is still its own carve-out, which
        # matters here more than above: `test_user`'s wire helper shares a bare
        # name with an `AdminActions` method, and the name-keyed graph merges
        # them.
        admin_ids = admin_shell_element_ids() - ADMIN_SHELL_EXIT_IDS
        shell_hits = kind_reach.element_ids_reached_by(
            kind_reach.test_function_name(item), admin_ids
        )
        if shell_hits:
            yield Verdict(
                DECLARED_ABSENCE,
                _GLOBAL_ADMIN_REASON.format(
                    why="drives the admin shell through the app UI: "
                    + _first_three(shell_hits)
                ),
                RULE_GLOBAL_ADMIN,
            )
        shell_fixtures = {}
        for name in sorted(closure - LIVE_SELF_GATED_FIXTURES - GLOBAL_ADMIN_FIXTURES):
            ids = kind_reach.element_ids_reached_by(name, admin_ids)
            if ids:
                shell_fixtures[name] = ids
        if shell_fixtures:
            names = ", ".join(sorted(shell_fixtures))
            ids = frozenset().union(*shell_fixtures.values())
            yield Verdict(
                DECLARED_ABSENCE,
                _GLOBAL_ADMIN_REASON.format(
                    why=f"fixture {names} drives the admin shell through the "
                    f"app UI: {_first_three(ids)}"
                ),
                RULE_GLOBAL_ADMIN,
            )

    return


def classify(item, mode: nest_mode_mod.NestMode) -> Verdict | None:
    """Why `item` must not run in `mode` — or `None` if it is eligible.

    The FIRST applicable rule, which is the right answer for the run: one
    exclusion, one reason, the most basic one the reader needs. Eligibility is a
    default, never an opt-in — this answers `None` unless something concrete
    excludes the test.
    """
    return next(_verdicts(item, mode), None)


def all_rules(item, mode: nest_mode_mod.NestMode) -> tuple:
    """Every rule that applies, in ratified order — the audit's question.

    Empty for an eligible test, and empty in standalone for everything except a
    `@pytest.mark.<mode>_only` marker, so the inner loop still pays nothing.
    """
    return tuple(verdict.rule for verdict in _verdicts(item, mode))


# ── The three declarations, for a fixture or action-layer caller ───────────

def mode_unbuilt(*, surface: str, detail: str = "", tracked: str = ""):
    """This mode could host `surface`, but the harness support is not built.

    Temporary debt. Skips normally; FAILS under `--strict-nest`. Either way the
    run tallies it, so the debt is printed rather than inferred from an `s`.

    Args:
        surface: what is missing, specific enough to grep for
                 ("DockerProvider handle_domain", "live account provisioning").
        detail:  why it is missing / what would close it.
        tracked: where the work is captured (a NEXT file, a goal-doc section).
    """
    mode = nest_mode_mod.run_mode()
    message = f"nest mode {mode.name!r} has not built {surface}"
    if detail:
        message += f" — {detail}"
    if tracked:
        message += f" (tracked: {tracked})"
    record_gate(mode.name, _current_test_id(), MODE_UNBUILT, message)
    if _STRICT_NEST:
        pytest.fail(
            f"--strict-nest: {message}\n"
            "This is unbuilt harness debt, not a structural absence: under "
            "--strict-nest a mode that cannot run a test must fail rather than "
            "skip, so a per-mode coverage claim stays falsifiable. Build the "
            "support, or — if this mode genuinely cannot ever host it — "
            "reclassify with declared_absence() and cite the goal doc.",
            pytrace=False,
        )
    pytest.skip(message)


def declared_absence(*, capability: str, doc: str):
    """A structural absence of this mode, declared in a goal doc. Always skips.

    `--strict-nest` honours this one: there is nothing to build, so failing
    would be noise that trains readers to ignore the flag.

    Args:
        capability: the absent capability in the goal doc's own words
                    ("a virgin box", "nest-side test hooks").
        doc:        the citation that declares it. REQUIRED — a declared absence
                    with no declaration is unbuilt debt wearing a better name.
    """
    if not doc or not doc.strip():
        raise ValueError(
            "declared_absence() requires a `doc` citation naming the goal-doc "
            "section that declares this absence. If no goal doc declares it, it "
            "is unbuilt harness debt — use mode_unbuilt() instead."
        )
    mode = nest_mode_mod.run_mode()
    message = f"nest mode {mode.name!r} has no {capability} (declared absence: {doc})"
    record_gate(mode.name, _current_test_id(), DECLARED_ABSENCE, message)
    pytest.skip(message)


def skip_environment(reason: str):
    """This box cannot host the mode right now — not a property of the test.

    A missing docker daemon, an absent image, an unreachable live box.
    `--strict-nest` ignores these by design: no harness work makes them run here.
    """
    mode = nest_mode_mod.run_mode()
    message = f"nest mode {mode.name!r} unavailable here: {reason}"
    record_gate(mode.name, _current_test_id(), SKIP_ENVIRONMENT, message)
    pytest.skip(message)


def _current_test_id() -> str:
    """The running test's node id, or `"<collection>"` outside a test.

    Best-effort and deliberately exception-proof: a tally that raises while
    reporting why something was skipped would turn a skip into an error.
    """
    try:
        return os.environ.get("PYTEST_CURRENT_TEST", "<collection>").split(" ")[0]
    except Exception:
        return "<collection>"
