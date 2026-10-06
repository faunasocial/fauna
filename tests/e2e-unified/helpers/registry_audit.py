"""Whole-frame assertions over an app's registry snapshot.

Owner: `docs/goal/architecture/e2e-conventions.md` § point 17 (assert general
invariants, not only hand-picked outcomes). The snapshot itself is
`PlatformDriver.registry_snapshot()`; this module is what turns it into a
verdict.

**Why a differential, and not a checklist.** The obvious whole-frame phrasing
for the offline gate is *"with no nest, every actuable control outside an
allow-list must be disabled"*. It is the phrasing the gate's own tracking issue
proposed, and it is the `focus-fanout == 1` mistake in a new costume: a page's
legitimately-offline controls are its tabs, its back button, its text fields,
its cancel buttons and every local toggle, so the allow-list would be most of
the page and would churn on every UI change. A checker that big cries wolf, gets
declared noise, and gets deleted.

The differential needs no list at all. Take the frame ONLINE, take it again
OFFLINE, and compare the controls present in both:

* **Monotonicity** — losing the nest must never make a control *live*. This is
  linux's "never *enables* what the page disabled" rule (`offline_gate.rs`)
  stated at frame scope, and it is true by construction of what a gate is: the
  verdict is `available`, and `.disabled(!available)` only ever subtracts.
  It catches an inverted polarity, and a control whose enablement is wired to
  the wrong half of the connection state — neither of which any single-control
  assertion on a hand-picked id can see.
* **Reach** — at least one control must actually have changed. A gate that
  no-ops passes every "this specific control is disabled" test that happens to
  pick a control disabled for its own reasons; measuring the frame says whether
  the gate did anything at all.

Neither half needs to know which wire kind a control issues, which is the fact
that lives in the Rust call chain and is unavailable to any frame-level checker.
That is deliberate: this asserts the *shape* of the gate's effect, and the
`kind` correctness stays with the fleet's own offline-gate-kinds check, which
reads the declaration where it is written.
"""

from __future__ import annotations

# A row's identity across two frames. NOT the id alone: repeated ids (list rows)
# are addressed by occurrence index, and the same id under two different scopes
# is two different controls. Matching on the same triple the driver addresses
# with means a finding here can be re-driven verbatim.
_KEY_FIELDS = ("scope", "id", "index")


def _key(row: dict) -> tuple:
    return tuple(row.get(f) for f in _KEY_FIELDS)


def describe(row: dict) -> str:
    """One control, in the spelling a driver call would take."""
    scope = row.get("scope") or ""
    where = f"{scope}/" if scope else ""
    return f"{where}{row.get('id')}[{row.get('index')}]"


def actuable_rows(snapshot) -> list[dict]:
    """The rows a driver can actually drive — clickable or typable.

    Read-only labels are excluded: they are the bulk of any frame and their
    enabled-ness is inherited decoration, never an affordance a user can be
    stranded by. Editable fields ARE included, because typing into a field whose
    save cannot happen is the same dead end as clicking a dead button.
    """
    return [r for r in snapshot if r.get("actuable") or r.get("editable")]


def enablement_delta(before, after) -> dict:
    """What changed between two frames of the same surface.

    Returns ``{"newly_disabled": [...], "newly_enabled": [...], "common": int}``,
    each list holding the ``after`` rows, ordered as the snapshot presented them.
    Only controls present in BOTH frames are compared — a control that appeared
    or vanished changed for reasons this comparison cannot attribute, and
    guessing is how a checker earns its reputation for noise.
    """
    prior = {_key(r): r for r in actuable_rows(before)}
    newly_disabled: list[dict] = []
    newly_enabled: list[dict] = []
    common = 0
    for row in actuable_rows(after):
        was = prior.get(_key(row))
        if was is None:
            continue
        common += 1
        if was.get("enabled") and not row.get("enabled"):
            newly_disabled.append(row)
        elif row.get("enabled") and not was.get("enabled"):
            newly_enabled.append(row)
    return {
        "newly_disabled": newly_disabled,
        "newly_enabled": newly_enabled,
        "common": common,
    }


def assert_offline_gate_reach(
    before,
    after,
    *,
    surface: str,
    allow_newly_enabled: frozenset[str] = frozenset(),
) -> dict:
    """Assert the two frame-scope properties of losing the nest, and report.

    ``before`` is the surface with a reachable nest, ``after`` the same surface
    without one; both are ``registry_snapshot()`` results. ``surface`` names the
    page for the failure message. ``allow_newly_enabled`` holds element **ids**
    that may legitimately come alive when the nest goes (a reconnect affordance
    would be the honest example) — empty today, and an addition to it owes a
    sentence saying why that control is live only offline.

    Returns the delta so a caller can assert more about it. Raises
    ``AssertionError`` naming every offender in full: a violation list that
    truncates reads as "these are the problems" when it is not, so nothing here
    caps its output.

    ``None`` for either frame means the app publishes no registry surface; the
    caller decides whether that is an ``n/a`` or a missing leg, because this
    module must never turn "I could not look" into "I looked and it was fine".
    """
    assert before is not None and after is not None, (
        f"{surface}: registry_snapshot() answered None — this app publishes no "
        "structured registry, so nothing was checked. Treat as n/a at the call "
        "site rather than reading this assertion's absence as coverage."
    )
    delta = enablement_delta(before, after)

    offenders = [
        r for r in delta["newly_enabled"] if r.get("id") not in allow_newly_enabled
    ]
    assert not offenders, (
        f"{surface}: losing the nest ENABLED {len(offenders)} control(s) that "
        "were disabled while it was reachable — a gate only ever subtracts, so "
        "this is an inverted verdict or an enablement wired to the wrong half of "
        "the connection state: "
        + ", ".join(describe(r) for r in offenders)
    )

    assert delta["common"], (
        f"{surface}: the two frames share no actuable control, so the comparison "
        "proved nothing — the surface was not the same page both times (a "
        "navigation, a relaunch, or a page that renders nothing without a nest)."
    )
    assert delta["newly_disabled"], (
        f"{surface}: {delta['common']} actuable control(s) were present in both "
        "frames and NOT ONE changed when the nest went away — the gate did "
        "nothing measurable on this surface. A per-control assertion can pass "
        "against this state whenever the control it picked is disabled for its "
        "own reasons."
    )
    return delta


def visible_page_tabs(driver) -> list[str]:
    """Every page tab this frame offers, read off the app's own registry, in
    frame order.

    The walk is over what the frame ACTUALLY offers (convention 17 — a general
    invariant over a systematic walk, not hand-picked pages; convention 3 — the
    page set is the app's, never a per-app list in the test), minus the one tab
    that is not a page: ``exit-tab`` quits the app. A tab counts only when it is
    actuable: an id that is merely present (a page's own presence anchor) is not
    something a user can press.
    """
    frame = driver.registry_snapshot()
    assert frame is not None, (
        f"{type(driver).__name__} serves no /registry frame, so a page walk "
        "cannot know which pages exist — implement the route rather than "
        "hand-listing tabs in the test"
    )
    tabs: list[str] = []
    for row in frame:
        element_id = row.get("id") or ""
        if (
            element_id.endswith("-tab")
            and element_id != "exit-tab"
            and row.get("actuable")
            and element_id not in tabs
        ):
            tabs.append(element_id)
    return tabs
