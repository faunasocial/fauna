"""The words the Windows setup wizard shows — read from the WiX sources, on every machine.

The MSI references WiX's stock `WixUI_FeatureTree` dialog set (`Package.wxs`), whose
licence page shows the file `WixUILicenseRtf` names and, when nothing names one, WiX's
built-in placeholder licence: a released installer once showed that placeholder. The
welcome page's sentence is `Strings.wxl`'s override, which reaches the MSI only through
`-loc`. Both are public texts (docs/goal/architecture/public-texts.md; the inventory's
`windows-installer-descriptions` entry), so no source here may carry a placeholder or
the word the product frame rules out. The painted page itself is read on Windows by the
installer suite; these pins hold the sources it is built from.
"""

from __future__ import annotations

import re
import xml.etree.ElementTree as ET
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_INSTALLER = Path(__file__).resolve().parents[3] / "apps" / "fauna-windows" / "installer"
_WIX = "{http://wixtoolset.org/schemas/v4/wxs}"
_WXL = "{http://wixtoolset.org/schemas/v4/wxl}"
#: A placeholder's tell, and the word the product frame rules out ("software", never
#: "platform" — public-texts.md § The review, item 3).
_BANNED = ("lorem", "ipsum", "platform")


def _licence_variable() -> str:
    found = []
    for wxs in sorted(_INSTALLER.glob("*.wxs")):
        for el in ET.parse(wxs).getroot().iter(f"{_WIX}WixVariable"):
            if el.get("Id") == "WixUILicenseRtf":
                found.append(el.get("Value", ""))
    assert len(found) == 1, (
        f"WixUILicenseRtf is set {len(found)} times in the installer sources; the stock "
        f"dialog set shows WiX's placeholder licence when it is unset")
    return found[0]


def _licence_path() -> Path:
    value = _licence_variable().replace("$(sys.SOURCEFILEDIR)", "")
    assert "$(" not in value, f"WixUILicenseRtf names {value!r}, which this pin cannot resolve"
    return _INSTALLER / value


def _rtf_text(rtf: str) -> str:
    """The words an RTF file shows: ignorable `{\\*…}` groups dropped, control words and
    braces stripped (enough for a plain file with no embedded objects)."""
    out, depth, skip_at = [], 0, None
    i = 0
    while i < len(rtf):
        ch = rtf[i]
        if ch == "{":
            depth += 1
            if rtf.startswith("{\\*", i) and skip_at is None:
                skip_at = depth
            i += 1
        elif ch == "}":
            if skip_at == depth:
                skip_at = None
            depth -= 1
            i += 1
        elif ch == "\\":
            m = re.match(r"\\([a-z]+)(-?\d+)? ?|\\'([0-9a-f]{2})|\\(.)", rtf[i:], re.I)
            if skip_at is None:
                if m.group(1) in ("par", "line"):
                    out.append("\n")
                elif m.group(3):
                    out.append(bytes.fromhex(m.group(3)).decode("cp1252"))
                elif m.group(4) in ("\\", "{", "}"):
                    out.append(m.group(4))
            i += m.end()
        else:
            if skip_at is None and ch not in "\r\n":
                out.append(ch)
            i += 1
    return "".join(out)


def test_the_licence_page_names_a_file_that_exists():
    path = _licence_path()
    assert path.is_file(), f"WixUILicenseRtf names {path.name}, which is not in the installer sources"


def test_the_licence_page_is_plain_rtf_with_the_fauna_licence():
    raw = _licence_path().read_text(encoding="ascii")
    assert raw.startswith("{\\rtf1"), "the licence page is not an RTF file"
    for embedded in ("\\object", "\\pict", "\\field"):
        assert embedded not in raw, f"the licence page embeds {embedded}; it must be plain RTF"
    shown = _rtf_text(raw)
    assert "MIT" in shown and "Apache License" in shown, shown


def test_no_installer_text_holds_a_placeholder_or_the_ruled_out_word():
    shown = {_licence_path().name: _rtf_text(_licence_path().read_text(encoding="ascii"))}
    for wxl in sorted(_INSTALLER.glob("*.wxl")):
        for el in ET.parse(wxl).getroot().iter(f"{_WXL}String"):
            shown[f"{wxl.name} {el.get('Id')}"] = el.get("Value") or (el.text or "")
    for where, text in shown.items():
        for word in _BANNED:
            assert word not in text.lower(), f"{where} shows {word!r}"


def test_the_licence_page_names_only_files_the_msi_installs_on_every_feature():
    """The licence page says the licence texts and the notices are installed with Fauna
    and names the files; each named file must be one `Notices.wxs` installs into
    INSTALLFOLDER, and its group must ride every top-level feature, else the sentence
    is false for some install (docs/goal/architecture/shipped-notices.md § The rule)."""
    shown = _rtf_text(_licence_path().read_text(encoding="ascii"))
    named = set(re.findall(r"\b(?:LICENSE-[A-Z]+|THIRD-PARTY-NOTICES)\.txt\b", shown))
    assert named, "the licence page names no installed licence file"
    group = next(
        g for g in ET.parse(_INSTALLER / "Notices.wxs").getroot().iter(f"{_WIX}ComponentGroup")
        if g.get("Id") == "NoticesComponents")
    assert group.get("Directory") == "INSTALLFOLDER"
    installed = {f.get("Name") for f in group.iter(f"{_WIX}File")}
    assert named <= installed, f"the licence page names {sorted(named - installed)}, not installed"
    package = ET.parse(_INSTALLER / "Package.wxs").getroot()
    for feature in package.iter(f"{_WIX}Feature"):
        if feature in {c for f in package.iter(f"{_WIX}Feature") for c in f.findall(f"{_WIX}Feature")}:
            continue
        refs = {r.get("Id") for r in feature.findall(f"{_WIX}ComponentGroupRef")}
        assert "NoticesComponents" in refs, f"feature {feature.get('Id')} installs no licence files"


def test_the_welcome_override_is_the_one_the_wizard_reads():
    wxl = _INSTALLER / "Strings.wxl"
    ids = {el.get("Id") for el in ET.parse(wxl).getroot().iter(f"{_WXL}String")}
    assert "WelcomeDlgDescription" in ids
