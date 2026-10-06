"""macOS itself evaluates the shipped app-floor predicate, and it really does gate.

`installers/macos.md` § Implementation status today (the floors note) makes the
minimum-macOS floors per-component: the service components run on 13.0, the
desktop app needs 15.0. The `.pkg` volume-check stays at the services floor so a
headless services-only install still reaches an older Mac mini, which leaves the
**app choice** to carry its own floor — `start_enabled`/`start_selected` gated on
an `appComponentSupported()` predicate in `distribution.xml`.

**Why this file exists rather than an assertion on the XML text.** A Distribution
predicate is JavaScript that only Apple's installer runs. Nothing in a build
type-checks it: misname `system.compareVersions`, compare a version string with
`>` instead, or return the wrong polarity, and `productbuild` still writes a
perfectly valid `.pkg` — the gate simply misbehaves on the user's machine, in the
one direction (silently disabling the app for everyone, or silently admitting a
box that cannot run it) that no one here would see. Asserting the XML *says*
`appComponentSupported()` proves only that we spelled our own string correctly.

So this hands the **actual shipped `<script>`** to the **actual `installer`
binary** and reads back what macOS decided, via `installer -showChoicesXML`
(read-only: it prints the evaluated choice tree and installs nothing, and needs
no privilege). Two probes, both built from the real script:

  * as shipped — must come back enabled and selected on this box, which is
    macOS 15 or newer by construction (it takes macOS 15 to build the app at
    all). Catches a predicate that errors or is wrongly false.
  * floor raised above this machine's OS — the same predicate with only the
    version literal changed, so it must come back DISABLED and UNSELECTED. This
    is the half no ≥15 machine can otherwise demonstrate, and it is the one that
    matters: it proves the attributes actually gate rather than being inert.

The probes carry no payload — `productbuild --distribution` alone, ~1s — so this
does not wait on the `.pkg` build and lives outside `test_installer.py` for
exactly that reason.

Tier: **tier_1**. None of Fauna's own binaries are involved — no nest, no
driver, no app — so by the taxonomy's question ("how much of OUR stack is
real?") this is an in-process configuration proof that happens to shell out to
two OS tools, the same way the tier_1 `test_apple_identifier_pins.py` shells out
to `git`. Keeping it out of tier_3 matters practically as well: a tier_3 marker
would make it take one of the machine's two e2e slots and queue behind real
installs for a check that costs a second. Its macOS-only skipif is structural
impossibility (convention 7's one permanent skip): `productbuild` and
`installer` exist nowhere else. `test_installer.py::TestDryRun` covers the
shipped `.pkg`'s own copy of the same attributes, and that one is tier_3
because it builds the real artifact.
"""
from __future__ import annotations

import os
import plistlib
import re
import subprocess
import sys

import pytest

pytestmark = [
    pytest.mark.skipif(sys.platform != "darwin", reason="macOS-only (productbuild/installer)"),
    pytest.mark.tier_1,
]

#: The floor the shipped predicate compares against — kept in sync with
#: `installers/macos.md` § Implementation status today (the floors note).
APP_MIN_OS = "15.0"
PREDICATE = "appComponentSupported()"
#: Higher than any macOS this could run on, so the raised-floor probe is false
#: on every machine rather than only on old ones.
ABOVE_ANY_OS = "999.0"


def _repo_root() -> str:
    here = os.path.dirname(os.path.abspath(__file__))
    out = subprocess.run(["git", "rev-parse", "--show-toplevel"],
                         capture_output=True, text=True, check=True, cwd=here)
    return os.path.normpath(out.stdout.strip())


def _shipped_script() -> str:
    """The `<script>` block of the real distribution.xml, verbatim.

    Lifted rather than retyped: a copy would drift, and a drifted copy would
    keep passing while the shipped predicate broke — which is the whole failure
    mode this file exists to catch.
    """
    path = os.path.join(_repo_root(), "installer", "macos", "distribution.xml")
    xml = open(path, encoding="utf-8").read()
    m = re.search(r"<script>(.*?)</script>", xml, re.DOTALL)
    assert m, (
        f"{path} has no <script> block, so the app-floor predicate is not defined "
        "there any more. If the gate moved, move this proof with it."
    )
    body = m.group(1).strip()
    # Unwrap the file's own CDATA section — re-wrapping it would nest `]]>` and
    # productbuild rejects the probe outright (measured while writing this).
    if body.startswith("<![CDATA[") and body.endswith("]]>"):
        body = body[len("<![CDATA["):-len("]]>")]
    assert "]]>" not in body, f"unexpected CDATA nesting in the shipped script: {body!r}"
    assert PREDICATE.rstrip("()") in body, (
        f"the shipped script no longer defines {PREDICATE}: {body!r}"
    )
    assert APP_MIN_OS in body, (
        f"the shipped predicate no longer compares against {APP_MIN_OS} — if the app "
        f"floor moved, update APP_MIN_OS here and the floors note in installers/macos.md: {body!r}"
    )
    return body


def _probe_pkg(tmp_path, script_body: str, name: str) -> str:
    """A payload-free product archive whose single choice is gated on the predicate."""
    dist = tmp_path / f"{name}.xml"
    dist.write_text(
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<installer-gui-script minSpecVersion="2">\n'
        f"    <title>{name}</title>\n"
        '    <options customize="always" require-scripts="false" hostArchitectures="arm64"/>\n'
        f"    <script><![CDATA[{script_body}]]></script>\n"
        '    <choices-outline><line choice="probe.app"/></choices-outline>\n'
        '    <choice id="probe.app" title="Probe"\n'
        f'            start_enabled="{PREDICATE}"\n'
        f'            start_selected="{PREDICATE}"/>\n'
        "</installer-gui-script>\n",
        encoding="utf-8",
    )
    pkg = tmp_path / f"{name}.pkg"
    out = subprocess.run(["productbuild", "--distribution", str(dist), str(pkg)],
                         capture_output=True, text=True, timeout=120)
    assert out.returncode == 0, f"productbuild rejected the probe:\n{out.stderr}"
    return str(pkg)


def _evaluated_choice(pkg: str) -> dict:
    """What macOS decided for the probe choice — `installer` prints, installs nothing."""
    out = subprocess.run(["installer", "-showChoicesXML", "-pkg", pkg, "-target", "/"],
                         capture_output=True, timeout=120)
    assert out.returncode == 0, (
        f"installer -showChoicesXML failed ({out.returncode}); a predicate that raises "
        f"shows up here:\n{out.stderr.decode(errors='replace')}"
    )
    tree = plistlib.loads(out.stdout)

    def walk(items):
        for item in items:
            if item.get("choiceIdentifier") == "probe.app":
                return item
            found = walk(item.get("childItems", []))
            if found:
                return found
        return None

    choice = walk(tree)
    assert choice is not None, f"probe.app missing from the evaluated choice tree: {tree}"
    return choice


def _this_macos() -> str:
    return subprocess.run(["sw_vers", "-productVersion"],
                          capture_output=True, text=True, check=True).stdout.strip()


def test_this_machine_is_at_or_above_the_app_floor():
    """The precondition the shipped-probe assertion below depends on.

    Stated as its own case so that a machine below the floor reports *that*,
    instead of the shipped probe failing and reading like a broken predicate.
    """
    version = _this_macos()
    major = int(version.split(".")[0])
    assert major >= int(float(APP_MIN_OS)), (
        f"this machine runs macOS {version}, below the app floor {APP_MIN_OS} — it cannot "
        "build the app either, so the shipped-probe expectation would be inverted here"
    )


def test_the_shipped_predicate_evaluates_and_admits_this_machine(tmp_path):
    """As shipped: enabled and selected on a box at or above the floor."""
    choice = _evaluated_choice(_probe_pkg(tmp_path, _shipped_script(), "asshipped"))
    assert choice["choiceIsEnabled"] is True, (
        "the shipped app-floor predicate came back FALSE on a machine at or above the "
        "floor — the Desktop App choice would be greyed out for every user. "
        f"Evaluated choice: {choice}"
    )
    assert choice["choiceIsSelected"] == 1, (
        f"the app choice evaluated unselected on a supported machine: {choice}"
    )


def test_raising_the_floor_above_this_machine_actually_disables_the_choice(tmp_path):
    """The half a modern machine cannot otherwise show: the gate really gates.

    Only the version literal changes, so this is the shipped predicate itself
    answering for a machine below its floor. If the attributes were inert — or
    the comparison had the wrong polarity — this comes back enabled.
    """
    raised = _shipped_script().replace(f"'{APP_MIN_OS}'", f"'{ABOVE_ANY_OS}'")
    assert ABOVE_ANY_OS in raised, "the floor literal was not substituted; check its quoting"
    choice = _evaluated_choice(_probe_pkg(tmp_path, raised, "raisedfloor"))
    assert choice["choiceIsEnabled"] is False, (
        "a choice gated above this machine's macOS came back ENABLED — the "
        "start_enabled gate is not gating, so an under-floor box could tick the "
        f"Desktop App and install an app it cannot launch. Evaluated choice: {choice}"
    )
    assert choice["choiceIsSelected"] == 0, (
        f"an under-floor choice came back selected: {choice}"
    )


def test_a_machine_exactly_AT_the_floor_is_admitted(tmp_path):
    """The boundary user: macOS exactly equal to the floor must be allowed in.

    `compareVersions` returns 0 for equal versions, so this is the case that
    separates `>= 0` from `> 0` — and `> 0` is the plausible typo, since it
    reads correctly ("newer than the floor") and is wrong only for the users
    sitting exactly on it. Neither other probe can see it: this machine is well
    above 15.0 and the raised-floor probe is well below its own. Substituting
    this machine's own version for the floor literal puts the shipped predicate
    on the boundary without needing a machine that sits there.

    Measured while writing this file: with only the other two probes, the
    `>= 0` -> `> 0` mutant SURVIVED. This case is what kills it.
    """
    here = _this_macos()
    at_floor = _shipped_script().replace(f"'{APP_MIN_OS}'", f"'{here}'")
    assert here in at_floor, "the floor literal was not substituted; check its quoting"
    choice = _evaluated_choice(_probe_pkg(tmp_path, at_floor, "atfloor"))
    assert choice["choiceIsEnabled"] is True, (
        f"a machine running exactly the floor version ({here}) was REFUSED the app "
        "choice — the comparison excludes the boundary (`> 0` where it must be `>= 0`), "
        f"so every user on exactly macOS {APP_MIN_OS} would be locked out. Evaluated: {choice}"
    )
    assert choice["choiceIsSelected"] == 1, (
        f"a machine exactly at the floor got the app choice unselected: {choice}"
    )
