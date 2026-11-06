"""A site link keeps the space before it.

Astro's HTML compression drops the whitespace between a line's last word and an
inline tag that opens the next line, so `released under our` + newline +
`<a …>code signing policy</a>` rendered as "ourcode signing policy" on the live
footer (and "Try the" + newline + `<a>meadow</a>` as "themeadow" on the 404 page).
The fix is to open the line with the word before the tag; this pins it on every
`.astro` source of the site.
"""

import re
from pathlib import Path

SITE_SRC = Path(__file__).resolve().parents[2] / "sites" / "fauna-social" / "src"

# A line that ends inside running text (a word or punctuation, not a tag or an
# expression), followed by a line that opens with an inline element.
_ENDS_IN_TEXT = re.compile(r"[A-Za-z0-9,.;:!?'\"’)]\s*$")
_OPENS_INLINE = re.compile(r"^\s*<(a|strong|em|b|i|code|span|abbr)[\s>]")


def glued_links(text: str) -> list:
    lines = text.splitlines()
    found = []
    for n, (prev, line) in enumerate(zip(lines, lines[1:]), start=2):
        stripped = prev.strip()
        if stripped.startswith(("//", "*", "/*", "<!--")) or not stripped:
            continue
        if _ENDS_IN_TEXT.search(prev) and _OPENS_INLINE.match(line):
            found.append((n, line.strip()))
    return found


def test_the_detector_catches_the_footer_shape():
    assert glued_links("  released under our\n  <a href=\"/x\">policy</a>.\n")
    assert not glued_links("  released under\n  our <a href=\"/x\">policy</a>.\n")
    assert not glued_links("  <p>\n  <a href=\"/x\">policy</a>\n")


def test_no_site_source_opens_a_line_with_an_inline_tag_after_running_text():
    offenders = []
    for path in sorted(SITE_SRC.rglob("*.astro")):
        for line_no, line in glued_links(path.read_text(encoding="utf-8")):
            offenders.append(f"{path.relative_to(SITE_SRC.parent)}:{line_no}: {line}")
    assert not offenders, (
        "Astro drops the space before an inline tag that opens a line after running "
        "text; move the word before the tag onto its line:\n" + "\n".join(offenders))
