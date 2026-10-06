"""Declared reasons an e2e test does not run on the app under test.

testing.md § Cross-app e2e conventions, convention 7. A test that quietly does
not run is worse than a missing test: it reports `s` inside a summary line
(`1 failed, 4 passed, 5 skipped`) that a hurried reader counts as fine, and
every session after inherits the impression that the app is covered. **A skip is
not coverage.**

Convention 7's only sanctioned skip is *structural impossibility*. Everything
else is one of three things, and this module makes each one say which:

    skip_unbuilt(...)      this app has not built the surface yet — TEMPORARY
                           parity debt. Skips normally; under `--strict-app` it
                           FAILS, and either way the run counts it and prints
                           the total, so the number is a ratchet that can only
                           go down.

    declared_absence(...)  a PERMANENT platform absence declared in a goal doc
                           (`apps/tui.md` § Declared platform absences — inline
                           AV playback, camera/QR capture, screen-reader tree,
                           drag-and-drop). Always skips, `--strict-app` included:
                           there is nothing to build, so failing would be noise.
                           Requires the goal-doc citation as an argument, so the
                           claim is checkable rather than asserted.

    skip_environment(...)  not about the app at all — the box cannot support this
                           run (no network, a domain that is registered, a
                           missing live credential). Always skips; `--strict-app`
                           deliberately ignores it, because no amount of app work
                           would make it run here.

The distinction is the whole point. `--strict-app` is only trustworthy if the
skips it fires on are exactly the ones an app *could* close by building
something — so a legitimate absence must stay a skip, and unbuilt debt must not.

Companion static half: `scripts/check_app_gate_ratchet.py` counts app-gated
skips that have NOT been routed through this module (down-only ratchet), so the
set stays enumerable without anyone maintaining a list by hand.
"""

from __future__ import annotations

import pytest

# ── Run-level state, set once by conftest's pytest_configure ────────────────
# The helpers are called from action classes and fixtures that have no `request`
# in scope, so the flag lives here and conftest pushes it in.
_STRICT_APP = False

# Every unbuilt-surface skip (or failure) this run reached, in hit order:
# (app, surface, detail). conftest's terminal summary prints the tally.
_UNBUILT_HITS: list[tuple[str, str, str]] = []


def set_strict_app(enabled: bool) -> None:
    """Called by conftest for `--strict-app`. Not for test code."""
    global _STRICT_APP
    _STRICT_APP = bool(enabled)


def strict_app_enabled() -> bool:
    return _STRICT_APP


def unbuilt_hits() -> list[tuple[str, str, str]]:
    """The run's unbuilt-surface hits, for the terminal summary."""
    return list(_UNBUILT_HITS)


def reset_unbuilt_hits() -> None:
    """Test-only: clear the tally between self-test cases.

    ⚠ **Pair it with :func:`restore_unbuilt_hits`, never leave it bare.** The
    tally is accumulated for the WHOLE run and read once, by
    `conftest.py::pytest_terminal_summary` — so a self-test that clears it
    mid-run and does not put the run's real hits back does not isolate itself,
    it TRUNCATES convention 7's visible count, and every genuinely-unbuilt
    surface reported before that module ran disappears from the summary. The
    count is supposed to be the visible, down-only ratchet a reader can trust
    (`e2e-conventions.md` § point 7); a self-test quietly editing it is the same
    class of hiding the convention exists to stop.
    """
    _UNBUILT_HITS.clear()


def restore_unbuilt_hits(hits) -> None:
    """Test-only: put a snapshot from :func:`unbuilt_hits` back.

    The other half of a self-test's isolation: snapshot before, clear as needed
    during, restore after — so the module's own fake-driver hits never reach the
    summary and the run's real ones always do.
    """
    _UNBUILT_HITS[:] = list(hits)


# ── The canonical app name of a driver ─────────────────────────────────────
# One implementation, used by both this module and app_capabilities.py.
# It is `is_*()`-based rather than class-name-substring-based on purpose: the
# substring form silently returned "unknown" for TuiDriver, which made every
# per-app capability lookup fall through to "not implemented" and skip — a whole
# category of tui coverage vanished with no signal at all (found 2026-07-29).
_PREDICATES = (
    # macos/ios before the generic checks: an Apple driver answers is_macos()
    # or is_ios() but the class names overlap.
    ("macos", "is_macos"),
    ("ios", "is_ios"),
    ("android", "is_android"),
    ("windows", "is_windows"),
    ("linux", "is_linux"),
    ("tui", "is_tui"),
    ("web", "is_web"),
)


def app_name(driver) -> str:
    """"tui" / "web" / "linux" / "windows" / "macos" / "ios" / "android".

    Accepts either a driver or the app id as a plain **string**. The string form
    exists so a test can declare an unbuilt surface *before* it launches
    anything — the launch-routing cases know their app from
    ``launch_harness.client`` while ``harness.driver`` is still ``None``, and a
    gate that had to launch first would spend a full app boot (~50s on windows)
    only to skip. Same tally, same ``--strict-app`` behavior either way.

    Falls back to the lowercased class name with a `driver` suffix stripped, so
    an unrecognised driver is still *named* in the skip reason instead of
    disappearing into "unknown".
    """
    if isinstance(driver, str):
        return driver
    for name, predicate in _PREDICATES:
        probe = getattr(driver, predicate, None)
        if callable(probe):
            try:
                if probe():
                    return name
            except Exception:  # a driver mid-teardown must not break reporting
                continue
    return type(driver).__name__.lower().removesuffix("driver") or "unknown"


# ── The three declarations ─────────────────────────────────────────────────
def skip_unbuilt(driver, *, surface: str, detail: str = "", tracked: str = ""):
    """This app has not built `surface` yet — temporary parity debt.

    Skips normally; FAILS under `--strict-app`. Either way the run tallies it.

    Args:
        driver:  the driver under test (names the app in the message).
        surface: what is missing, in ui.yaml/goal-doc vocabulary — an element id,
                 a page, or a named capability (`"admin-dns page"`,
                 `"attachment-button"`). Specific enough that a reader can grep
                 for it.
        detail:  why it is missing / what would close it.
        tracked: where the work is captured (a NEXT file, a goal-doc section).
    """
    app = app_name(driver)
    _UNBUILT_HITS.append((app, surface, detail))
    message = f"{app} has not built {surface}"
    if detail:
        message += f" — {detail}"
    if tracked:
        message += f" (tracked: {tracked})"
    from helpers import feature_ledger
    feature_ledger.note_skip_class(feature_ledger.UNBUILT)
    if _STRICT_APP:
        pytest.fail(
            f"--strict-app: {message}\n"
            "This is unbuilt-surface debt, not a platform absence: under "
            "--strict-app an app that cannot run a test must fail rather than "
            "skip, so a coverage claim for this app stays falsifiable. Build the "
            "surface, or — if it is genuinely permanent — reclassify it with "
            "declared_absence() and cite the goal doc that declares it.",
            pytrace=False,
        )
    pytest.skip(message)


def declared_absence(driver, *, capability: str, doc: str):
    """A permanent platform absence declared in a goal doc. Always skips.

    `--strict-app` honours this one: there is no surface to build, so failing
    would be noise that trains readers to ignore the flag.

    Args:
        capability: the absent capability, in the goal doc's own words.
        doc:        the citation that declares it, e.g.
                    "apps/tui.md § Declared platform absences". REQUIRED — a
                    declared absence with no declaration is just unbuilt debt
                    wearing a better name.
    """
    if not doc or not doc.strip():
        raise ValueError(
            "declared_absence() requires a `doc` citation naming the goal-doc "
            "section that declares this absence. If no goal doc declares it, it "
            "is unbuilt debt — use skip_unbuilt() instead."
        )
    app = app_name(driver)
    from helpers import feature_ledger
    feature_ledger.note_skip_class(feature_ledger.ABSENCE)
    pytest.skip(f"{app} declares no {capability} (declared absence: {doc})")


def skip_unless_optimistic_launch_entry(driver):
    """A session-destroying launch verdict only TEARS A SESSION DOWN on an app
    that entered optimistically. Skips (declared) on every app that gates.

    `iOS is the only one`. It renders the home tabs off the cached identity and
    lets the machine's verdict correct course, so `WizardAt` / `IdentityChanged`
    / the account-index refusal arrive at a shell the user is already inside and
    end a real session. Every other app holds its shell on a launch-gate or
    wizard state and authenticates nothing until the verdict lands, so those same
    verdicts route a wizard and destroy nothing — there is no teardown to observe
    and none to count.

    That makes this a `declared_absence`, not `skip_unbuilt`: the six gating apps
    are not missing a surface someone could build, they are structurally
    incapable of reaching the event, and `--strict-app` must not red them. The
    asymmetry is deliberate and named as such in the goal doc (*"the one
    deliberate macOS/iOS launch divergence"*).

    ⚠ **What this does NOT say.** It does not say the other apps never re-enter a
    launch — they do, from the account switcher, the remove-account promote and
    the factory-reset re-onboard, which is exactly why the
    verdict-ownership rule is all-app (`onboarding.md` § App-launch routing →
    *A verdict renders only while its own launch is still the current one*).
    It says only that on those apps such a verdict has no *authenticated session*
    of its own to drop.

    Takes a driver **or** the app id as a plain string, so a caller can gate
    before it launches anything (`app_name`'s string form).
    """
    if app_name(driver) == "ios":
        return
    declared_absence(
        driver,
        capability=(
            "an optimistic launch entry, so a session-destroying launch verdict "
            "has no authenticated session of its own to tear down here and "
            "counts no teardown"
        ),
        doc=(
            "docs/goal/behavior/onboarding.md § App-launch routing — \"iOS is "
            "the only app that enters optimistically … the other six GATE\"; "
            "§ Implementation status today, the iOS row — \"That is the one "
            "deliberate macOS/iOS launch divergence\""
        ),
    )


def skip_if_no_local_search_arm(driver, *, content_class: str):
    """`Contact` and `File` search hits can only reach an app with a LOCAL arm.

    A search result of those classes lives only in backend 2, the app's sealed
    local index. The nest arm (backend 1) is `content_fts`, which has exactly
    two writers — the post projection and `index_profile` — so no contact- or
    file-class row can ever exist there (`bins/fauna-nest/src/storage/sealed.rs`).

    On **web** that makes those result sets empty by construction, permanently:
    web registers no local arm and never will — no Tantivy on wasm, a structural
    property rather than parity debt (`libs/fauna-wasm/src/search.rs`:
    `has_local_index()` on a wasm manager "is permanently `false`"; the search
    page's own comment says the same). So this is a `declared_absence` and not
    `skip_unbuilt`: there is no surface to build, and `--strict-app` must not red
    it. Same call the sibling suite already makes for `SearchNav::Mail`
    (`test_search_local_index.py`).

    ⚠ Do NOT extend this to the `Post` leg: posts are served by the nest arm, so
    web does receive them and that leg is a real assertion there.

    Being in a test's per-app tuple answers a *different* question — "does this
    app act on a search result's navigation target?" — and web answers yes. It
    is the class that cannot arrive, not the handling that is missing, which is
    why both legs read as a seeding or search-UI bug for a 90 s timeout each
    until 2026-08-24.

    **The phone seats are gated too, for a DIFFERENT reason — kept worded apart
    on purpose** (2026-08-25). web has no local
    *arm*; iOS and Android have a full one and query it exactly like a desktop.
    What they lack is a *builder*: `CLIENT_BUILDS_INDEX` is `false` on those
    targets (`libs/fauna-ffi/src/index_launch.rs`), so a phone renders segments
    a **desktop** seat published and synced. A single-seat e2e run has no such
    seat, so content the phone seeds itself is never indexed by anyone and no
    `Contact`/`File` row can arrive — the same empty result set as web, reached
    by a different road. Conflating the two in one message would send a reader
    hunting for a missing wasm arm on a platform that has one.
    """
    app = app_name(driver)
    if app == "web":
        declared_absence(
            driver,
            capability=(
                f"local search arm, so a `{content_class}` search hit can never "
                "reach this app's result list"
            ),
            doc=(
                "docs/goal/ui/search.md § Implementation status today — \"web has "
                "no local arm by design (§ State & data shape — off on wasm, "
                "structurally) and never will\""
            ),
        )
    skip_if_seat_builds_no_index(driver, what=f"a `{content_class}` hit")
    # windows registers the local arm on every Search-page load
    # (SearchResultsPage.xaml.cs) —
    # joining tui/macOS/iOS/android/linux, so it needs no branch here.


def skip_if_seat_builds_no_index(driver, *, what: str):
    """A phone seat queries a local index but never BUILDS one. Declared skip
    on iOS/Android; a no-op on every other app.

    `CLIENT_BUILDS_INDEX` is `false` on the phone targets
    (`libs/fauna-ffi/src/index_launch.rs`): the query side is registered there,
    the builder is not, so a phone renders segments a desktop seat published and
    synced (`content-index.md` § Build vs. query). A SINGLE-seat journey whose
    hit is content this seat seeded itself — a draft it wrote, a message it
    received, mail it filed — is therefore never indexed by anyone and can never
    be found there. That is the ratified split, not a surface an app session
    could build, so it survives `--strict-app`.

    `what` names the content the journey needs found, in the test's own words.
    Web has no local ARM at all — a different absence, declared by
    `skip_if_no_local_search_arm` — so this helper leaves web alone.
    """
    if app_name(driver) not in ("ios", "android"):
        return
    declared_absence(
        driver,
        capability=(
            f"index BUILDER of its own, so {what} — content this single seat "
            "seeded itself — is never indexed by anyone and can never reach its "
            "result list"
        ),
        doc=(
            "docs/goal/behavior/content-index.md § Build vs. query — \"a "
            "desktop app (or the MDA bridge during a session) builds and "
            "syncs; phones receive the segments and query the synced copy\"; "
            "CLIENT_BUILDS_INDEX is false on iOS/Android and \"the phone-only "
            "foreground-incremental fallback remains unbuilt\""
        ),
    )


def skip_environment(reason: str):
    """The box cannot support this run — not an app property. Always skips.

    For a missing live credential, an unreachable network, a domain that turns
    out to be registered. `--strict-app` ignores these by design: no app work
    would make them run here.
    """
    from helpers import feature_ledger
    feature_ledger.note_skip_class(feature_ledger.ENVIRONMENT)
    pytest.skip(f"environment: {reason}")
