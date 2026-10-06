"""Post-frame invariant checker — convention 17's layer (b).

Owner: `docs/goal/architecture/e2e-conventions.md` § point 17 (ratified
2026-08-03). Layer (a) is the tier_1 in-process walk (built on tui and linux);
this is the layer that rides **every existing e2e test** and asserts a small
catalogue of GENERAL invariants over the driver-visible state surface after
each test's final frame — "this alone would have caught the collision bug on
any test that ever visited Settings".

**Why a hand-authored suite needs it.** Every hand-written test drives one path
and asserts one hand-picked outcome, so a defect in a state nobody wrote a test
for violates no assertion any test makes. Three live-user-found tui bugs
survived a green 1147-test suite exactly that way. This checker inverts the
economics: it observes the frame that ~1100 existing tests already navigate to,
for the cost of one state read each.

**Shape, copied deliberately from `conftest.py::_post_reset_surface_probe`.**
Env-gated (off by default, costs nothing), fixture-attached, writes to a FILE
rather than stdout, and *never fails the run it observes*. The file matters for
the same reason it does there, and more so: the test that ends on a violating
frame usually **passes** — the violation is latent, and pytest swallows fixture
prints on passing tests.

**Staged rollout** (the discipline convention 17 borrows from apple's
`--strict-enabled`): observe-and-log first, promote to failing after triage.
Observation is the **default** (see below); `FAUNA_E2E_FRAME_INVARIANTS=<path>`
redirects the corpus and an off-token switches it off;
`FAUNA_E2E_FRAME_INVARIANTS_STRICT=1` additionally fails the test. Nothing is
promoted yet — a checker that cries wolf gets deleted.

**Why observation is default-on, since 2026-08-15.** It shipped off-unless-named
that morning, with promotion gated on *"a clean full `--app sweep`"*. Both halves
were measured the same day and the arrangement could not converge:
`--app sweep --collect-only` collects **3757 tests** on ONE dev machine's app set
(2026 tier_3), and the other two machines carry their own sweeps — a day-scale
scheduled act, not something a session performs as a prerequisite; meanwhile,
off-by-default meant the fleet's
continuous e2e running contributed **zero** frames toward the very corpus the
promotion was gated on. So the gate is now an **accumulated** clean corpus, which
the fleet meets by working, and the promotion switch stays exactly where it was.
The observing-not-failing half — the reason default-on is safe — is untouched.

**The three adoption disciplines, as they bind HERE.**

* *Invariants are few and true-by-construction.* Each one below names the bug
  class it catches, and — the rule that actually does the work — **must not fire
  on any reachable legitimate state**. `focus-fanout` is the cautionary tale: the
  obvious phrasing is "exactly one line paints focused", and it is WRONG. A
  fresh authenticated tui session starts in `Zone::Sidebar`, where the page's
  focus ring does not have the keyboard and `focused_line_count` legitimately
  reads 0 (`tests/test_tui_settings_focus.py` documents exactly this, and calls
  `switch_pane` to get off it). A `== 1` checker would have fired on most tests
  in the suite, been declared noise, and been deleted — which is why the bound
  is `<= 1`.

* *An app that does not publish a field is `n/a`, never a violation.* The
  checker rides all 7 apps from day one, and a missing key is never a defect on
  the frame in front of us.

  ⚠ **`n/a` does not by itself mean "this app owes a twin" — for `focus-fanout`
  it is a permanent, reasoned property.** tui is the only app that paints its
  own focus affordance, because it is the only one without a toolkit that owns
  focus: GTK/DOM/WinUI/SwiftUI/Compose each guarantee at most one focused
  element per root, so the identity-collision class `focus-fanout` catches
  cannot arise there at all. Audited 2026-08-15: linux only ever *requests*
  focus (`grab_focus`, never a hand-painted highlight), web uses declarative
  `class:selected` on unique value keys, and no app but tui publishes the field.
  The generalised class is *"an affordance painted by comparing on a key that is
  not unique per element"* — tui's was `element.id`, blank on ~19 Settings rows
  at once; the GUI apps' equivalents compare on genuinely unique keys (a date
  tuple, a calendar id) and clear the class on non-matches. So do **not** read a
  `focus-fanout` `n/a` as a queued leg; the portable catalogue for a toolkit app
  is the two shape invariants.

* *Red-verification is part of adoption.* Every invariant here was demonstrated
  to catch a temporarily-reverted real defect or an equivalent shape mutation
  before it counted as coverage; the evidence, including the defect the
  verification found in this very module, is recorded in
  `e2e-conventions.md` § Implementation status today.
"""

from pathlib import Path

# The corpus a run appends to when nothing redirects it: one file per checkout,
# beside the harness that writes it, gitignored. Per-checkout rather than
# machine-global on purpose — it is the *session's* own evidence, it dies with
# the checkout, and two sessions never interleave each other's frames. Size is
# bounded by arithmetic rather than by a cap: ~120 bytes a line × the ~2400
# app-bearing tests of one machine's full sweep is ~300 KB per sweep.
DEFAULT_CORPUS_NAME = ".frame-invariants.log"

# Turning the probe off must stay one env var away. A probe that cannot be
# switched off is one bad day from being deleted from the harness outright —
# the fate the `== 1` phrasing of `focus-fanout` would have earned.
_OFF_TOKENS = frozenset({"", "0", "off", "no", "false", "none"})

# How many violating tests the end-of-run summary names before it stops being a
# summary. A build broken in a shared way violates on every frame; the corpus is
# where the full list lives.
SUMMARY_DETAIL_CAP = 5


def resolve_corpus_path(env_value, harness_dir):
    """Where this run's frames go: ``Path``, or ``None`` for "do not observe".

    ``env_value`` is ``FAUNA_E2E_FRAME_INVARIANTS`` as read from the
    environment (``None`` when unset), ``harness_dir`` the directory holding the
    e2e harness. Unset observes into [`DEFAULT_CORPUS_NAME`] there; an off-token
    disables; anything else is taken as the path verbatim, which is the contract
    the variable had before observation became the default.
    """
    if env_value is None:
        return Path(harness_dir) / DEFAULT_CORPUS_NAME
    if env_value.strip().lower() in _OFF_TOKENS:
        return None
    return Path(env_value)


class RunTally:
    """What one pytest run saw, so the run can report its own verdict.

    A corpus file nobody opens is the same as no observation, and the session
    that *causes* a violation is the one that can cheapest explain it — but
    pytest swallows fixture prints on passing tests, and the test that ends on a
    violating frame usually passes (that is the whole thesis: the violation is
    latent). The end-of-run terminal summary is the one surface that is neither
    swallowed nor a failure, so the verdict rides there.
    """

    def __init__(self):
        self.observed = 0
        self.unobserved = 0
        self.violated = []

    def record(self, nodeid, violations, unobserved=None):
        """Fold one frame in. ``unobserved`` is the reason it could not be READ."""
        if unobserved:
            self.unobserved += 1
            return
        self.observed += 1
        if violations:
            self.violated.append((nodeid, list(violations)))

    def summary(self, path):
        """The terminal-summary lines for this run; empty when nothing was seen.

        A run that observed no frame at all — a tier_1-only run, or one where the
        probe never fired — says nothing rather than growing a line about a probe
        that did not run. But a run whose frames all went UNOBSERVED **does**
        report: a wholly broken probe announcing a clean run is worse than no
        probe, because it is believed. Same refusal [`format_line`] makes per
        frame, at run scope.

        [`format_line`]: format_line
        """
        if not self.observed and not self.unobserved:
            return []
        head = (
            f"frame invariants: {self.observed} frames observed, "
            f"{len(self.violated)} violated"
        )
        if self.unobserved:
            head += f", {self.unobserved} unobserved"
        lines = [f"{head} — corpus: {path}"]
        for nodeid, violations in self.violated[:SUMMARY_DETAIL_CAP]:
            lines.append(f"  VIOLATION[{'; '.join(violations)}] {nodeid}")
        remaining = len(self.violated) - SUMMARY_DETAIL_CAP
        if remaining > 0:
            lines.append(f"  … and {remaining} more — grep VIOLATION {path}")
        return lines


# Which fixture's frame gets probed when a test holds several drivers. An
# ORDERING, never a membership test: a driver under a name absent from this list
# is still found (see [`driver_for`]), because the alternative failure mode is a
# probe silently reporting a clean run it never looked at.
PROBE_FIXTURE_PREFERENCE = ("logged_in_app", "admin_app", "app", "persistent_app")


def _as_driver(value):
    """The driver in ``value`` — the value itself, its ``.driver``, or ``None``.

    A driver is what the harness *drives*: it answers both ``get_state`` and
    ``reset``. Requiring the pair keeps a snapshot object that happens to expose
    a ``get_state`` from being probed as if it were an app.
    """
    if value is None:
        return None
    inner = getattr(value, "driver", None)
    if callable(getattr(inner, "get_state", None)) and callable(getattr(inner, "reset", None)):
        return inner
    if callable(getattr(value, "get_state", None)) and callable(getattr(value, "reset", None)):
        return value
    return None


def driver_for(funcargs):
    """The driver whose last frame this test left, or ``None`` for a test with none.

    ``funcargs`` is pytest's ``item.funcargs`` — every fixture VALUE the test
    actually resolved, which is the only honest answer to "whose frame is this":
    layer (b) originally hung off the conftest ``app`` fixture's own teardown,
    and the first real run measured what that reached — **nothing**, across 9
    green tests, because `test_navigation.py` defines its own class-level ``app``
    over ``persistent_app`` and shadows it. Seven modules shadow ``app`` that way
    and ten touch ``persistent_app``, among them the state-protocol and
    UI-honesty modules, which leave the frames a walker most wants to see. So the
    probe resolves by value and rides every driver-bearing test, whatever built
    the driver.
    """
    for name in PROBE_FIXTURE_PREFERENCE:
        driver = _as_driver(funcargs.get(name))
        if driver is not None:
            return driver
    for _, value in sorted(funcargs.items()):
        driver = _as_driver(value)
        if driver is not None:
            return driver
    return None


# Distinct from a published ``null``. `state.get(k)` collapses "this app does not
# publish k" and "k is present and null" into the same `None`, and those are
# opposite verdicts: the first is a queued per-app leg (`n/a`), the second is the
# field having lost its shape (a violation). Found by red-verification — the
# `messages` mutation reported `n/a` instead of firing, because the mutated value
# happened to serialize as null.
_ABSENT = object()


def _check_focus_fanout(state):
    """At most one line may paint with the focus highlight.

    Bug class: **focus-paint identity collision.** tui's paint code compared
    focus by element *id* string; the Settings rail builds ~19 of its 21 rows
    with a deliberately empty id, so every blank-id row matched at once and the
    whole rail painted highlighted together. A live human found it; the suite
    could not, because id-less rows are absent from the id-keyed automation
    registry by construction. Fixed by a POSITION comparison.

    ⚠ The bound is `<= 1`, deliberately — see the module docstring. 0 is a
    legitimate, extremely common state (focus in the sidebar zone, or a page
    with no focusable rows), and asserting `== 1` is how this checker would
    have been deleted for crying wolf.
    """
    count = state.get("focused_line_count", _ABSENT)
    if count is _ABSENT:
        return None
    if not isinstance(count, int) or isinstance(count, bool):
        return f"focused_line_count is {count!r}, not an integer"
    if count > 1:
        return f"{count} lines paint focused at once (at most 1 may)"
    return ""


def _check_messages_readable(state):
    """The message surface keeps its shape, so a failure can still diagnose itself.

    Bug class: **the error surface losing its shape.** Convention 2 gives every
    page an `error-message` element and convention 6 requires failures to
    diagnose themselves — both route through `error_text()`/`has_error()`, which
    read this state. If `messages` stops being a mapping, or `error` stops being
    text, every one of those reads degrades silently and the whole suite's
    self-diagnosis goes quiet at once.

    Note what is NOT asserted: an error being *present* is entirely legitimate
    (many tests assert one). Only the readability of the surface is invariant.
    """
    messages = state.get("messages", _ABSENT)
    if messages is _ABSENT:
        return None
    if not isinstance(messages, dict):
        return f"messages is {type(messages).__name__}, not a mapping"
    if "error" in messages:
        err = messages["error"]
        if err is not None and not isinstance(err, str):
            return f"messages.error is {type(err).__name__}, not text or null"
    return ""


def _check_nav_stack(state):
    """The nav stack is non-empty and every entry names a view.

    Bug class: **a nav stack that names nowhere.** The dead-end and stale-sub-
    page families both leave the app on a surface the stack does not describe;
    an empty stack, or a top entry with no `view`, is the structural signature.
    It matters because navigation assertions read *through* this — a test that
    checks "we are on the events page" against a stack that names nothing at all
    fails for a reason nobody can read, and one that only checks side effects
    passes outright.
    """
    nav = state.get("nav", _ABSENT)
    if nav is _ABSENT:
        return None
    if not isinstance(nav, dict):
        return f"nav is {type(nav).__name__}, not a mapping"
    stack = nav.get("stack", _ABSENT)
    if stack is _ABSENT:
        return None
    if not isinstance(stack, list):
        return f"nav.stack is {type(stack).__name__}, not a list"
    if not stack:
        return "nav.stack is empty — the app is somewhere the stack does not name"
    for i, entry in enumerate(stack):
        if not isinstance(entry, dict):
            return f"nav.stack[{i}] is {type(entry).__name__}, not a mapping"
        view = entry.get("view")
        if not isinstance(view, str) or not view:
            return f"nav.stack[{i}] names no view (view={view!r})"
    return ""


# The receive-loop exits that mean the rail is DEAD, not done: a native loop
# that died by panic, and a web pump whose pass outran its ceiling
# (`fauna_e2e_agent::CONV_RECEIVE_CYCLES_KEY` owns the vocabulary).
_DEAD_RECEIVE_EXITS = frozenset({"panicked", "stalled"})
# The designed exits — the session was dropped, or the engine handed over.
_DESIGNED_RECEIVE_EXITS = frozenset({"closed", "retired"})


def _check_receive_loop_alive(state):
    """The receive loop has not died underneath the frame.

    Bug class: **a dead receive rail.** A panic inside a receive pass stops
    every inbound message and mail until a restart nobody is told to do. On
    web a `RefCell` borrow panicked inside every receive pass from 2026-09-09
    to 2026-09-12: every real web session's rail was dead for three days, and
    it was found by one journey's first run — not by the suite, whose tests
    ended on frames of a dead app and passed, because none asserts delivery.

    ⚠ It fires on the exit REASON, never on bare `ended`. A loop legitimately
    leaves when its session closes or its engine is handed over (`closed`,
    `retired`), and counters sit at zero before any session exists — a "loop
    alive" check on `ended` or on the counters would fire on every account
    switch and every pre-login frame, and be deleted for crying wolf.

    `n/a` for an app that publishes no `conv_receive_cycles` mapping — absent,
    or the published `null` the key's own contract reserves for "this app has
    no leg" — and for one whose mapping carries no `exit` field yet.
    """
    cycles = state.get("conv_receive_cycles", _ABSENT)
    if not isinstance(cycles, dict):
        return None
    exit_ = cycles.get("exit", _ABSENT)
    if exit_ is _ABSENT:
        return None
    if exit_ is None or exit_ in _DESIGNED_RECEIVE_EXITS:
        return ""
    if exit_ in _DEAD_RECEIVE_EXITS:
        return (
            f"the receive loop is dead (exit={exit_}) — no message or mail "
            "reaches this app until it restarts"
        )
    return f"conv_receive_cycles.exit is {exit_!r}, not a known exit"


def _check_region_block_never_silent(state):
    """A region ``Block`` verdict never renders silent.

    Owner: `region-blocking.md` § What the build owes in tests — "wherever the
    engine says `Block` for the region source, the placeholder is present on
    that surface" — invariant 1 of that doc (a withheld item is shown as
    withheld, by whom and why; it never simply vanishes).

    Bug class: **silent withholding.** A render arm that drops a region-blocked
    item — a surface routed through the composed engine whose Block arm returns
    nothing, a new surface that honours the verdict but forgot the placeholder —
    looks, to a test that only checks the body is gone, exactly like success.

    The app publishes two counts for its last painted frame, taken on opposite
    sides of the render: ``blocked`` at the verdict (items the composed engine
    blocked with the region as the source) and ``placeholders`` at the element
    registration (``region-blocked-notice`` elements painted for a block).
    Fewer placeholders than blocks is the violation. More is legitimate — the
    counts are per frame and a placeholder may be painted by a surface that
    reuses a verdict computed elsewhere.

    `n/a` for an app that publishes no ``region_block_render`` mapping.
    """
    counts = state.get("region_block_render", _ABSENT)
    if counts is _ABSENT:
        return None
    if not isinstance(counts, dict):
        return f"region_block_render is {type(counts).__name__}, not a mapping"
    blocked = counts.get("blocked")
    placeholders = counts.get("placeholders")
    for name, value in (("blocked", blocked), ("placeholders", placeholders)):
        if not isinstance(value, int) or isinstance(value, bool):
            return f"region_block_render.{name} is {value!r}, not an integer"
    if placeholders < blocked:
        return (
            f"{blocked} item(s) region-blocked this frame but only {placeholders} "
            "region-blocked-notice placeholder(s) painted — a region block rendered silent"
        )
    return ""


# The catalogue. Few on purpose: each entry is a global property of ANY frame,
# each names the bug class it catches, and none fires on a legitimate state.
# Adding one is an adoption event, not an edit — it owes a red-verification.
INVARIANTS = (
    ("focus-fanout", _check_focus_fanout),
    ("messages-readable", _check_messages_readable),
    ("nav-stack", _check_nav_stack),
    ("receive-loop-alive", _check_receive_loop_alive),
    ("region-block-never-silent", _check_region_block_never_silent),
)


def evaluate(state):
    """Run the catalogue over one frame's state.

    Returns ``(violations, not_applicable)`` — the first a list of
    ``"<name>: <detail>"`` strings, the second the names of invariants this app
    publishes no field for. A ``None`` state (no agent, app not up) yields no
    violations and marks everything ``n/a``: the checker observes frames, and
    the absence of one is the fixture's business, never a frame violation.
    """
    if not isinstance(state, dict):
        return [], [name for name, _ in INVARIANTS]
    violations, not_applicable = [], []
    for name, check in INVARIANTS:
        try:
            result = check(state)
        except Exception as exc:  # a checker must never fail the run it observes
            violations.append(f"{name}: checker raised {type(exc).__name__}: {exc}")
            continue
        if result is None:
            not_applicable.append(name)
        elif result:
            violations.append(f"{name}: {result}")
    return violations, not_applicable


def format_line(nodeid, violations, not_applicable, outcome=None, unobserved=None):
    """One tab-separated trace line, in the reset probe's grep-friendly shape.

    ``outcome`` carries the test's own verdict when the runner knows it. A
    violation on a frame left by an ALREADY-FAILING test is usually a
    consequence, not a finding, and triage that cannot tell them apart drowns —
    so the verdict is recorded rather than reasoned about here.

    ``unobserved`` is the reason the frame could not be READ at all (a dead
    bridge, an app already gone). It gets its own verdict word rather than
    riding "OK", because a run where every frame went unobserved and a run where
    every frame was clean are the same file otherwise — and the first is a
    broken probe reporting success, which is the one failure mode a checker like
    this must never have.
    """
    if unobserved:
        verdict = f"UNOBSERVED[{unobserved}]"
    elif violations:
        verdict = "VIOLATION[" + "; ".join(violations) + "]"
    else:
        verdict = "OK"
    if not_applicable and not unobserved:
        verdict += f" n/a={','.join(not_applicable)}"
    if outcome and outcome != "passed":
        verdict += f" test={outcome}"
    return f"{verdict}\t{nodeid}\n"
