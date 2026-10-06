"""The screen-capture posture's rule 1, pinned at the source.

`docs/goal/architecture/security.md` § On-screen secret exposure (screen
capture), ratified 2026-08-15. Three ordered rules; **rule 1 wins where they
collide** and is the one worth a pin, because it forbids the obvious
"completion" of the feature:

> Never suppress capture on a root-secret surface. `secret-key-display` and
> `recovery-kit-secret-display` show key material that is client-only-resident:
> a user who loses it loses the account, with no recovery path. Users
> legitimately screenshot a recovery kit because it is the copy that saves them.

So the failure this module exists to catch is a well-meaning follow-up session
"finishing the job" by extending suppression to the identity secret or the
recovery kit — a change that looks like more security and is in fact the
irreversible harm, and one that no per-app unit test is positioned to see: on
apple the root-secret screens live in the per-TARGET onboarding views
(`Fauna-macOS/`, `Fauna-iOS/`) while the suppression modifier lives in the
shared `FaunaKit` package, so a FaunaKit test cannot observe them at all.

Pure text analysis — no build, no driver, no Apple toolchain, which is the
point: the artifact-level witnesses for this feature are per-platform (android's
`ScreenCaptureWindowFlagTest` needs Robolectric, apple's needs a Mac), so
without a toolchain-free pin the rule is only enforced on the machine that
happens to own that app.
"""

import re
from functools import lru_cache
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]

# The two root-secret element ids. `security.md` § On-screen secret exposure
# names exactly these; they are the ids, not the file names, because a screen
# can move between files and the id is what ui.yaml pins.
_ROOT_SECRET_IDS = ("secret-key-display", "recovery-kit-secret-display")

# The three minted, revocable credentials that rule 2 covers. Kept beside the
# forbidden set so the two lists cannot drift apart silently.
#
# The mail app-password reveal paints on the Connected apps roster
# (`connected-apps-item-secret`) in every app of `_ALL_THREE_SURFACE_APPS`
# (docs/goal/ui/connected-apps.md — the credentials list left Mail & Calendar),
# so one shared tuple serves them all.
_MINTED_CREDENTIAL_IDS = (
    "connected-apps-item-secret",
    "atproto-app-credential-reveal",
    "nostr-bunker-connect-string",
)

# Per-app suppression call spellings. A new app's leg adds its own row here in
# the same commit that lands it; linux, web and tui are rule-3 declared absences
# with no platform API to reach for, so they have nothing to name.
_SUPPRESSION_CALLS = {
    "apps/fauna-apple": r"suppressScreenCapture",
    "apps/fauna-android": r"SuppressScreenCapture|FLAG_SECURE",
    "apps/fauna-windows": r"SuppressScreenCapture|SetWindowDisplayAffinity",
}

# `.xaml` is here for windows and only windows: it is the one app that keeps
# element ids in MARKUP while the suppression call sits in code-behind. Without
# it the rule-1 scan reads a `.xaml.cs` that names no id and the rule-2 scan
# reads a `.xaml` that makes no call, and BOTH directions go quietly vacuous for
# windows — green while covering nothing, the failure mode every grep-shaped
# assertion has. See `_units` for the pairing that closes it.
_SOURCE_SUFFIXES = {".swift", ".kt", ".cs", ".xaml"}


def _code_only(text: str) -> str:
    """`text` with whole-line and block comments removed.

    Needed because the most safety-conscious file in the tree is the one that
    names the forbidden ids most often: each app's suppression implementation
    carries a rule-1 warning comment listing exactly the two surfaces it must
    never be attached to. Scanning raw text flags those — i.e. it punishes the
    documentation of the rule.

    Only WHOLE-line comments go, never a trailing `// …` on a code line: a
    trailing comment naming an id can at worst raise a false alarm, while
    stripping `//` mid-line would eat a URL inside a string literal and turn a
    real violation invisible. False alarm is the direction this rule wants.

    XAML's `<!-- … -->` is stripped on the same terms and for the same reason:
    windows keeps its ids in markup, and its markup is where the rule-1 warning
    comments naturally go.
    """
    out, in_block, in_xml = [], False, False
    for line in text.splitlines():
        stripped = line.strip()
        if in_block:
            if "*/" in stripped:
                in_block = False
            continue
        if in_xml:
            if "-->" in stripped:
                in_xml = False
            continue
        if stripped.startswith("/*"):
            in_block = "*/" not in stripped
            continue
        if stripped.startswith("<!--"):
            in_xml = "-->" not in stripped
            continue
        if stripped.startswith(("//", "*", "///")):
            continue
        out.append(line)
    return "\n".join(out)


def _sources(app_dir: str) -> list[Path]:
    root = _REPO / app_dir
    return [
        p
        for p in sorted(root.rglob("*"))
        if p.suffix in _SOURCE_SUFFIXES
        # Generated UniFFI bindings and the staged xcframework copies mention
        # element ids in doc comments; they render nothing.
        and "generated" not in p.parts
        and "Generated" not in p.parts
        and ".build" not in p.parts
        # MSBuild output. Excluded so the verdict does not depend on whether
        # this machine happens to have built the windows app.
        and "bin" not in p.parts
        and "obj" not in p.parts
        and not any(part.endswith(".xcframework") for part in p.parts)
        and "FaunaFFISwift" not in p.parts
    ]


@lru_cache(maxsize=None)
def _id_spellings(app_dir: str) -> dict[str, tuple[str, ...]]:
    """Every way a source under `app_dir` can name an element id.

    ⚠ **This is what keeps the module from going silently vacuous, and it has
    already happened once.** The original pin (2026-08-16) matched the literal
    kebab-case id. The element-id constant sweep the very next day
    moved apple's views to `Ids.mailSettingsCredentialItemSecret`,
    leaving the literal only in `Generated/UiIds.swift` — which this module
    deliberately excludes. From that commit on, apple's arms scanned zero real
    files. The vacuity guard below is what caught it; without that guard the
    whole module would simply have gone quiet.

    So instead of hard-coding a spelling, read the app's own generated id map —
    every app emits `<CONSTANT> = "<literal-id>"` on one line, whatever its
    casing convention (`mailSettingsCredentialItemSecret` on apple,
    `MailSettingsCredentialItemSecret` on windows,
    `MAIL_SETTINGS_CREDENTIAL_ITEM_SECRET` on android) — and accept either the
    literal or the constant. A future app that adopts constants inherits the
    coverage instead of quietly losing it.
    """
    spellings: dict[str, set[str]] = {}
    for generated in sorted((_REPO / app_dir).rglob("UiIds.*")):
        for name, value in re.findall(
            r'([A-Za-z_][A-Za-z0-9_]*)\s*(?::[^=]+)?=\s*"([a-z0-9][a-z0-9-]*)"',
            generated.read_text(encoding="utf-8", errors="ignore"),
        ):
            spellings.setdefault(value, set()).add(name)
    return {value: tuple(sorted(names)) for value, names in spellings.items()}


def _params(app_dirs, *, with_call: bool):
    """Parametrization whose ids carry NO bare app token.

    ⚠ **Not cosmetic — this is the second way this module went silently
    vacuous.** `conftest.py::_parametrized_clients` used to split a param id on
    `-` and treat any known-app token it found as "this case exercises that
    app", then deselect it when the app was not in `--app`. An id of
    `apps/fauna-android` split to `{android}`, so android's arm was deselected
    out of every default run since it was written (`apps/fauna-apple` survived
    only by the accident that `apple` is not an app name — `macos` and `ios`
    are). Adding a windows row would have inherited exactly the same fate. The
    matcher now keys on the real fixture behind each `callspec` entry rather
    than the id string — this direct `@pytest.mark.parametrize`
    is excluded regardless of its ids today — but the underscore-joined ids
    below are kept rather than reverted, both to document the incident and
    because there is no reason for these ids to carry a bare app token either
    way.

    That deselection defeats the module's whole reason for existing: it is the
    *toolchain-free* pin, and `security.md` § Implementation status today says
    why — "the artifact-level witnesses are per-platform, so without it the rule
    is enforced only on whichever machine happens to own that app." A rule-1
    violation on android must fail on a Windows dev box that cannot build
    android; that is the point. These are source-text audits, not app-driver
    tests, so they must run everywhere.

    Hence ids with `_` rather than `-`: no token, no app match, no deselection.
    """
    out = []
    for app_dir in app_dirs:
        label = app_dir.rsplit("/", 1)[-1].replace("fauna-", "") + "_sources"
        values = (app_dir, _SUPPRESSION_CALLS[app_dir]) if with_call else app_dir
        out.append(pytest.param(*(values if with_call else (values,)), id=label))
    return out


def _mentions(text: str, app_dir: str, element_id: str) -> bool:
    """Does `text` name `element_id`, by literal or by generated constant?"""
    if element_id in text:
        return True
    return any(
        re.search(rf"\b{re.escape(const)}\b", text)
        for const in _id_spellings(app_dir).get(element_id, ())
    )


def _units(app_dir: str) -> list[tuple[str, str]]:
    """`(label, code)` pairs — the granularity every assertion below scans at.

    A "unit" is one screen's worth of source. On apple and android that is a
    single file, so this is just `_sources` with its comments stripped. On
    windows a screen is a PAIR: `Foo.xaml` paints the ids, `Foo.xaml.cs` makes
    the calls. Scanning them apart is what would make both directions of this
    module vacuous for windows — the rule-1 half would never see a file that
    both names a root secret and suppresses, because no windows file does
    either alone.

    Deliberately no attempt to be cleverer than file-level (or here,
    screen-level): an approximation that errs toward *false alarm* is the right
    direction for a rule whose violation costs a user their account, and real
    layouts keep root secrets on their own screens anyway.
    """
    grouped: dict[str, list[Path]] = {}
    for path in _sources(app_dir):
        # `Foo.xaml.cs` has suffix `.cs` and stem `Foo.xaml` — pair it with the
        # markup it is code-behind for.
        key = path.as_posix()
        if key.endswith(".xaml.cs"):
            key = key[: -len(".cs")]
        grouped.setdefault(key, []).append(path)
    units = []
    for key, paths in sorted(grouped.items()):
        code = "\n".join(
            _code_only(p.read_text(encoding="utf-8", errors="ignore")) for p in paths
        )
        units.append((Path(key).relative_to(_REPO).as_posix(), code))
    return units


@pytest.mark.parametrize("app_dir,call", _params(sorted(_SUPPRESSION_CALLS), with_call=True))
def test_no_root_secret_surface_suppresses_capture(app_dir, call):
    """Rule 1, the one that wins on collision.

    A file that paints a root-secret id must not also call the app's capture
    suppression. This is deliberately file-level rather than
    expression-level: an approximation that errs toward *false alarm* is the
    right direction for a rule whose violation costs a user their account, and
    the real layouts keep root secrets on their own screens anyway.
    """
    offenders = []
    for label, text in _units(app_dir):
        if not any(_mentions(text, app_dir, rid) for rid in _ROOT_SECRET_IDS):
            continue
        if re.search(call, text):
            offenders.append(label)
    assert not offenders, (
        "these sources render a ROOT-SECRET surface and call capture suppression "
        "in the same file. `security.md` § On-screen secret exposure rule 1 forbids "
        "it: the identity secret and the recovery kit are client-only-resident, so "
        "blocking the screenshot trades a shoulder-surfing risk for account loss — "
        f"the irreversible one. Offenders: {offenders!r}"
    )


@pytest.mark.parametrize("app_dir,call", _params(sorted(_SUPPRESSION_CALLS), with_call=True))
def test_the_root_secret_ids_are_still_rendered_somewhere(app_dir, call):
    """Vacuity guard for the rule-1 pin above.

    If the ids were renamed or the screens moved, the pin would scan zero files
    and go permanently green while covering nothing — the failure mode of every
    grep-shaped assertion.

    ⚠ This asserts that *at least one* of the two ids is painted, not both, and
    that weakness is load-bearing rather than an oversight: **windows renders
    only `secret-key-display`** (`Views/Onboarding/IdentityCreatedView.xaml`) —
    it has no recovery-kit surface yet. Demanding both would fail windows today
    for a screen it has not built, and demanding neither is what leaves the pin
    vacuous. So: one is enough to prove the scan reaches real screens, and the
    honest statement of windows' coverage is "rule 1 is pinned on the identity
    secret here, and gains the recovery kit when that screen lands".
    """
    rendering = [
        label
        for label, text in _units(app_dir)
        if any(_mentions(text, app_dir, rid) for rid in _ROOT_SECRET_IDS)
    ]
    assert rendering, (
        f"no source under {app_dir} renders {_ROOT_SECRET_IDS!r} at all, so the rule-1 "
        "pin is asserting nothing. The ids were renamed or the screens moved — fix "
        "the pin, do not delete it."
    )


# The apps whose leg covers ALL THREE minted surfaces, so the rule-2 direction
# can be asserted exhaustively against them. android is deliberately absent: its
# leg mounts on two (mail + Bluesky) and it pins them at the platform-bit level
# in its own `ScreenCaptureWindowFlagTest`, which is stronger than a source grep.
_ALL_THREE_SURFACE_APPS = ("apps/fauna-apple", "apps/fauna-windows")


@pytest.mark.parametrize("app_dir", _params(_ALL_THREE_SURFACE_APPS, with_call=False))
def test_every_minted_credential_reveal_suppresses_capture(app_dir):
    """Rule 2, from the other side: the surfaces that SHOULD hold one, do.

    Without this, deleting the modifier from a reveal is invisible — rule 1's
    pin passes more easily the less suppression exists anywhere.
    """
    call = _SUPPRESSION_CALLS[app_dir]
    units = _units(app_dir)
    missing = []
    for cred_id in _MINTED_CREDENTIAL_IDS:
        painters = [text for label, text in units if _mentions(text, app_dir, cred_id)]
        assert painters, (
            f"no source under {app_dir} renders {cred_id!r} — this pin has gone vacuous "
            "for it"
        )
        if not any(re.search(call, text) for text in painters):
            missing.append(cred_id)
    assert not missing, (
        f"these minted-credential reveals no longer suppress screen capture on {app_dir} "
        "(`security.md` § On-screen secret exposure rule 2 — suppress while, and only "
        f"while, a minted credential is actually revealed): {missing!r}"
    )
