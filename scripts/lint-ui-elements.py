#!/usr/bin/env -S uv run --quiet
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyyaml"]
# ///
"""Lint: verify all 7 apps implement every element from ui.yaml.

Reads the `elements` list from each page in ui.yaml and checks that each
client's source contains the corresponding test ID pattern.

Also validates component elements when --components is passed (or by default).
Components define reusable UI groups with their own element lists and child
components. The lint resolves children recursively and checks each element
against all applicable clients.

Handles dynamic patterns where IDs are constructed at runtime:
- Kotlin:  testTag("${item.route}-tab")
- Swift:   accessibilityIdentifier("\\(item.rawValue)-tab")
- Svelte:  data-testid="inbox-mode-{m.value}"
- Rust:    set_test_id(&btn, &test_id) with format!("{}-tab", ...)

Usage: python3 scripts/lint-ui-elements.py [--verbose] [--components] [--no-components]
"""
import functools
import re
import sys
import yaml
from pathlib import Path

#: ui.yaml is 868 KB, and PyYAML's pure-Python loader parses it in ~2.7 s against
#: ~0.2 s for the libyaml-backed one — the gap between "cheap merge gate" and most
#: of it. `getattr` because CSafeLoader exists only where PyYAML was built with
#: libyaml (true on Windows, 2026-09-05); where it is not, this is the loader the
#: file always used. Same idiom, same reason, as the ui-actual accuracy checker.
_YAML_LOADER = getattr(yaml, "CSafeLoader", yaml.SafeLoader)

sys.path.insert(0, str(Path(__file__).resolve().parent))
import ui_id_names  # noqa: E402  (sibling module — the id/constant-name owner)

ROOT = Path(__file__).resolve().parent.parent
UI_YAML = ROOT / "tests" / "e2e-unified" / "ui.yaml"

# For each client: (pattern template with {id} placeholder, search directory, file extensions)
APPS = {
    "web": {
        "pattern": 'data-testid="{id}"',
        "dir": "apps/fauna-web/src",
        "exts": {".svelte", ".ts", ".js"},
    },
    "windows": {
        "pattern": 'AutomationId="{id}"',
        "dir": "apps/fauna-windows",
        "exts": {".xaml", ".cs"},
    },
    "linux": {
        "pattern": '"{id}"',
        "dir": "apps/fauna-linux/src",
        "exts": {".rs"},
    },
    "ios": {
        "pattern": 'accessibilityIdentifier("{id}")',
        "dir": "apps/fauna-apple/Fauna-iOS",
        "exts": {".swift"},
    },
    "macos": {
        "pattern": 'accessibilityIdentifier("{id}")',
        "dir": "apps/fauna-apple/Fauna-macOS",
        "exts": {".swift"},
    },
    "android": {
        "pattern": 'testTag("{id}")',
        "dir": "apps/fauna-android",
        "exts": {".kt"},
    },
    "tui": {
        "pattern": '"{id}"',
        "dir": "apps/fauna-tui/src",
        "exts": {".rs"},
    },
}

# Shared code that counts for both iOS and macOS
SHARED_DIRS = {
    "ios": [("apps/fauna-apple/FaunaKit", {".swift"})],
    "macos": [("apps/fauna-apple/FaunaKit", {".swift"})],
}

# Generated files that sit inside a scanned app tree and are FULL of id-shaped or
# otherwise quoted strings which are not call sites. The bare-string fallback must skip
# them or it reads a table of constants as evidence that the app implements every id in
# it. The i18n `strings.*` files are the original members; the `UiIds.*` files added
# 2026-08-16 are worse, because each one literally contains *every* declared element id —
# leaving them in made `ui-actual-lint` report ids as "PRESENT in <app> source" for apps
# that render nothing of the sort.
GENERATED_STRING_FILES = frozenset({
    "apps/fauna-apple/FaunaKit/Sources/FaunaExtensionKit/Generated/L.swift",
    "apps/fauna-web/src/lib/i18n/strings.ts",
    "apps/fauna-windows/FaunaApp/FaunaApp/Strings/en-US/Resources.resw",
    "apps/fauna-android/app/src/main/res/values/i18n_strings.xml",
    "libs/fauna-i18n/src/strings.rs",
    "tests/e2e-unified/i18n/strings.py",
    # scripts/ui-ids-generate.py targets — see build-system.md § Generated element-id
    # constants. Listed in full, including the two outside any scanned app dir, so a
    # future dir move cannot silently reintroduce the contamination.
    "libs/fauna-ui-ids/src/lib.rs",
    "apps/fauna-web/src/lib/generated/uiIds.ts",
    "apps/fauna-apple/FaunaKit/Sources/FaunaKit/Generated/UiIds.swift",
    "apps/fauna-android/app/src/main/kotlin/social/fauna/generated/UiIds.kt",
    "apps/fauna-windows/FaunaApp/FaunaApp/Generated/UiIds.cs",
    "tests/e2e-unified/generated/ui_ids.py",
})

# Comment syntax across every source language we scan. Swift/Kotlin/Rust/TS/JS/C#
# use `//` to end-of-line and `/* … */` (incl. the `/** … */` doc form) for blocks;
# Svelte and XAML use `<!-- … -->`. All three are stripped before the bare-string
# fallback: an id named in prose is documentation, not an implementation of it.
# The block/markup arms close the same hole the `//` arm was added for, one syntax
# over — a Swift `///` doc block and a Kotlin `/** … */` KDoc are the same claim.
# They fix no entry as of 2026-08-10 (measured: zero delta) — every quoted id in a
# block comment today is also implemented for real. Kept because the hole is real
# and the next `/** "some-id" */` is what it stops; `_check_dynamic_prefix` below
# is the loose matcher that DOES false-positive today (see its own note).
_LINE_COMMENT_RE = re.compile(r"//[^\n]*")
_BLOCK_COMMENT_RE = re.compile(r"/\*.*?\*/", re.DOTALL)
_MARKUP_COMMENT_RE = re.compile(r"<!--.*?-->", re.DOTALL)


@functools.lru_cache(maxsize=None)
def _is_generated_strings(path: Path) -> bool:
    """Is `path` one of the generated i18n string tables?

    A pure function of the path, and asked once per (id-check x file): 43 277
    times in one `ui-actual-lint` run, each paying a `Path.relative_to` — 2.74 s
    of the 4.21 s gate on Windows, 2026-09-05, for an answer that cannot change
    within a run. The cache is unbounded on purpose: its key space is the files
    under the app trees, which `_get_cached` is already holding in memory.
    """
    try:
        return path.relative_to(ROOT).as_posix() in GENERATED_STRING_FILES
    except ValueError:
        return False


def _strip_comments(content: str) -> str:
    """Blank out line, block and markup comments.

    Block/markup comments go first: a `//` inside a `/* … */` body is already
    comment text, and stripping lines first could leave a dangling `*/`.
    """
    content = _BLOCK_COMMENT_RE.sub("", content)
    content = _MARKUP_COMMENT_RE.sub("", content)
    return _LINE_COMMENT_RE.sub("", content)


def _bare_string_match(content: str, element_id: str) -> bool:
    """True if `"{element_id}"` or `'{element_id}'` appears outside a comment."""
    bare_double = f'"{element_id}"'
    bare_single = f"'{element_id}'"
    if bare_double not in content and bare_single not in content:
        return False
    stripped = _strip_comments(content)
    return bare_double in stripped or bare_single in stripped


def _build_file_cache(search_dir: Path, exts: set) -> dict[Path, str]:
    """Read all matching files into memory for fast searching."""
    cache = {}
    if not search_dir.exists():
        return cache
    for f in search_dir.rglob("*"):
        if f.is_file() and f.suffix in exts:
            try:
                cache[f] = f.read_text(encoding="utf-8", errors="replace")
            except (OSError, UnicodeDecodeError):
                continue
    return cache


# Cache of directory contents to avoid re-reading files per element
_dir_cache: dict[str, dict[Path, str]] = {}


def _get_cached(search_dir: Path, exts: set) -> dict[Path, str]:
    key = f"{search_dir}|{'|'.join(sorted(exts))}"
    if key not in _dir_cache:
        _dir_cache[key] = _build_file_cache(search_dir, exts)
    return _dir_cache[key]


def _check_dynamic_prefix(element_id: str, files: dict[Path, str]) -> bool:
    r"""Check if element_id is dynamically constructed from a prefix + variable.

    Detects patterns like:
      testTag("${item.route}-tab")  where item.route = "feed"  → "feed-tab"
      "\(item.rawValue)-tab"        where rawValue = "feed"    → "feed-tab"
      "inbox-mode-{m.value}"        where m.value = "open"     → "inbox-mode-open"
      format!("{}-tab", name)       where name = "feed"        → "feed-tab"

    Strategy: split element_id on the last '-' to get (prefix, suffix).
    Then check if the source has a template using that prefix with a dynamic
    interpolation that could produce the suffix.
    """
    # Try splitting at each '-' from the right to find dynamic prefix patterns
    parts = element_id.split("-")
    if len(parts) < 2:
        return False

    for split_at in range(len(parts) - 1, 0, -1):
        prefix = "-".join(parts[:split_at])
        suffix = "-".join(parts[split_at:])

        # Convert kebab-case to possible identifiers for enum case matching
        # e.g. "allow_knock" → "allow_knock", "allowKnock"
        # e.g. "contacts_only" → "contacts_only", "contactsOnly"
        prefix_variants = {prefix, prefix.replace("-", "_")}
        suffix_variants = {suffix, suffix.replace("-", "_")}

        # Patterns that construct IDs dynamically using the prefix
        dynamic_patterns = [
            # Kotlin string template: "${...}-suffix"
            re.compile(r"\$\{[^}]+\}" + re.escape(f"-{suffix}") + r'["\)]'),
            # Swift interpolation: "\(...)-suffix"
            re.compile(r'\\\([^)]+\)' + re.escape(f"-{suffix}")),
            # Swift interpolation: "prefix-\(...)"
            re.compile(re.escape(f"{prefix}-") + r'\\\([^)]+\)'),
            # Svelte template: "prefix-{expr}"
            re.compile(re.escape(f"{prefix}-") + r'\{[^}]+\}'),
            # Rust format: "prefix-{}" or "{}-suffix"
            re.compile(re.escape(f"{prefix}-") + r'\{\}'),
            # `{}` must open the quoted literal itself — else `{}` matches
            # some OTHER template's dynamic middle segment (e.g. "cal-{}-text"
            # dynamically fills a color, not this id's whole prefix) and any
            # id sharing that suffix false-positives as already built.
            re.compile(r'["\'`]\{\}' + re.escape(f"-{suffix}")),
        ]

        # Check if any file has a dynamic template that could produce this ID
        has_template = False
        for content in files.values():
            for pat in dynamic_patterns:
                if pat.search(content):
                    has_template = True
                    break
            if has_template:
                break

        if not has_template:
            continue

        # Template found — now check if ANY file has the value that would
        # produce this specific ID (can be in a different file)
        all_content = "\n".join(files.values())
        for v in prefix_variants | suffix_variants:
            if f'"{v}"' in all_content or f"'{v}'" in all_content:
                return True
            # Swift/Kotlin enum case names (e.g. "case feed", "case allow_knock")
            if re.search(rf'\bcase\s+{re.escape(v)}\b', all_content):
                return True
            # Kotlin DrawerItem("feed", ...)
            if re.search(rf'DrawerItem\(\s*"{re.escape(v)}"', all_content):
                return True
    return False


def _check_dynamic_padded_prefix(element_id: str, files: dict[Path, str]) -> bool:
    r"""Check for a string literal that starts with `<element_id>-` followed
    directly by a formatted/padded placeholder — the shape every app uses for
    a zero-padded indexed suffix (`events-time-slot-{HH-MM}`,
    `events-day-cell-{YYYY-MM-DD}`), which `_check_dynamic_prefix` above
    misses because it only recognizes bare `{}`/`\\(...)`, not a format
    specifier baked into the same placeholder:
      Rust:   format!("events-time-slot-{:02}-{:02}", h, m)
      C#:     $"events-day-cell-{dayDate:yyyy-MM-dd}"
      Swift:  String(format: "events-time-slot-%02d-%02d", h, m)
      Kotlin: "events-time-slot-${slot.format(SLOT_ID_HHMM)}"
      Svelte: `events-day-cell-${dateKey(cell.date)}` / "events-time-slot-{slot.hh}-{slot.mm}"

    Anchors on the id immediately following an opening quote/backtick (so a
    prose mention mid-string, e.g. inside a doc comment, can't match after
    comment-stripping) and a placeholder-start token right after the
    trailing hyphen.
    """
    needle = f"{element_id}-"
    for path, content in files.items():
        if _is_generated_strings(path):
            continue
        stripped = _strip_comments(content)
        start = 0
        while True:
            idx = stripped.find(needle, start)
            if idx == -1:
                break
            start = idx + 1
            if idx == 0 or stripped[idx - 1] not in "\"'`":
                continue
            tail = stripped[idx + len(needle) : idx + len(needle) + 4]
            if tail[:1] in ("{", "%", "$"):
                return True
            if tail[:2] == "\\(":
                return True
    return False


def find_id_in_dir(
    element_id: str, pattern: str, search_dir: Path, exts: set, app_name: str = ""
) -> bool:
    """Check if element_id exists in any file under search_dir.

    Handles literal patterns, generated-constant references, and dynamic patterns.
    The bare-string fallback skips auto-generated i18n files and line comments (e.g.
    Swift `///` doc blocks) to avoid false positives from string constants or example
    snippets.

    The constant form is not optional politeness: once an app renders through
    `scripts/ui-ids-generate.py`'s constants there is no literal left to find, so an
    adopted app would report as implementing *nothing at all* — a total false alarm
    that looks exactly like a catastrophic regression. `scripts/ui_id_names.py` owns
    the spellings so this check and the generator cannot drift.
    """
    target = pattern.format(id=element_id)
    const_targets = ui_id_names.reference_forms(app_name, element_id) if app_name else []
    files = _get_cached(search_dir, exts)

    for path, content in files.items():
        # Exact call-site match — trust this in all files, including i18n.
        if target in content:
            return True
        # A reference to the generated constant is equally an implementation.
        if any(const in content for const in const_targets):
            return True
        # Bare-string fallback — skip i18n files and strip line comments.
        if _is_generated_strings(path):
            continue
        if _bare_string_match(content, element_id):
            return True

    # Check for dynamic prefix construction
    if _check_dynamic_prefix(element_id, files):
        return True
    if _check_dynamic_padded_prefix(element_id, files):
        return True

    return False


def find_id_in_app(element_id: str, app_name: str) -> bool:
    """Check if element_id exists in the app's source or shared code."""
    cfg = APPS[app_name]
    pattern = cfg["pattern"]
    primary = ROOT / cfg["dir"]

    if find_id_in_dir(element_id, pattern, primary, cfg["exts"], app_name):
        return True

    # Check shared directories
    for shared_dir, shared_exts in SHARED_DIRS.get(app_name, []):
        if find_id_in_dir(element_id, pattern, ROOT / shared_dir, shared_exts, app_name):
            return True

    return False


def _find_id_strict_in_dir(
    element_id: str, pattern: str, search_dir: Path, exts: set, app_name: str = ""
) -> bool:
    """Strict presence: literal call-site pattern, generated-constant reference, or
    bare quoted string only — NO dynamic-prefix inference.

    The loose `find_id_in_dir` accepts an id when the client has *any* matching
    template (e.g. ``{}-tab``) and the bare token appears anywhere — fine for
    route-derived tabs, but it false-positives an explicit id like ``admin-tab``
    on every client that has a ``*-tab`` template plus the ubiquitous word
    "tab". Gated entries are placed as explicit literals on each client, so they
    must be matched strictly. (Mirrors lint-ui-actual.py's strict check.)

    A reference to the generated constant counts here for the same reason it counts
    in the loose matcher — and it is the *more* explicit form, not a looser one:
    ``ids::CONTACT_CONFIRM`` can only exist because ui.yaml declares
    ``contact-confirm``, whereas a bare string can be any coincidence. This path is
    the one page/sub-page elements take, so leaving it literal-only is what makes an
    adopted app report page elements as missing while it renders them correctly —
    16 such phantom gaps appeared on tui the moment it adopted.
    """
    target = pattern.format(id=element_id)
    const_targets = ui_id_names.reference_forms(app_name, element_id) if app_name else []
    files = _get_cached(search_dir, exts)
    for path, content in files.items():
        if target in content:
            return True
        if any(const in content for const in const_targets):
            return True
        if _is_generated_strings(path):
            continue
        if _bare_string_match(content, element_id):
            return True
    return False


def find_id_strict_in_app(element_id: str, app_name: str) -> bool:
    """Strict variant of find_id_in_app — literals and constants, no inference."""
    cfg = APPS[app_name]
    pattern = cfg["pattern"]
    primary = ROOT / cfg["dir"]
    if _find_id_strict_in_dir(element_id, pattern, primary, cfg["exts"], app_name):
        return True
    for shared_dir, shared_exts in SHARED_DIRS.get(app_name, []):
        if _find_id_strict_in_dir(element_id, pattern, ROOT / shared_dir, shared_exts, app_name):
            return True
    return False


def _resolve_component_elements(
    comp_name: str,
    components: dict,
    _visited: set | None = None,
) -> list[str]:
    """Recursively collect all element IDs for a component and its children."""
    if _visited is None:
        _visited = set()
    if comp_name in _visited:
        return []
    _visited.add(comp_name)

    comp = components.get(comp_name, {})
    elements = list(comp.get("elements", []))
    for child in comp.get("children", []):
        elements.extend(_resolve_component_elements(child, components, _visited))
    return elements


_NOT_IMPL_RE = re.compile(r"^not\s+(yet\s+)?implemented|^not\s+applicable", re.IGNORECASE)


def _excluded_apps(comp: dict) -> dict[str, str]:
    """Return {app: reason} for apps marked not-implemented in notes.

    Handles per-client notes like ``ios: "Not yet implemented."`` and the
    ``others`` key which applies to all clients not explicitly listed.
    """
    notes_raw = comp.get("notes", {})
    # ``notes`` is usually a {client: note} mapping, but a few component
    # defs use a single descriptive string — treat that like an ``others``
    # note so the not-implemented heuristic still applies (and we don't
    # crash on ``str.items()``).
    if isinstance(notes_raw, str):
        notes = {"others": notes_raw}
    elif isinstance(notes_raw, dict):
        notes = notes_raw
    else:
        notes = {}
    excluded: dict[str, str] = {}
    explicitly_listed = set()

    for key, value in notes.items():
        if key == "others":
            continue
        if key in APPS:
            explicitly_listed.add(key)
            if _NOT_IMPL_RE.search(str(value)):
                excluded[key] = str(value)

    # "others" applies to every client not explicitly mentioned
    others_note = notes.get("others", "")
    if others_note and _NOT_IMPL_RE.search(str(others_note)):
        for client in APPS:
            if client not in explicitly_listed:
                excluded[client] = str(others_note)

    return excluded


def lint_components(spec: dict, verbose: bool = False):
    """Lint component elements and return (errors, warnings, checked_count).

    Returns:
        errors: list of (client, component, element_id) — hard failures
        warnings: list of (client, component, element_id, reason) — not-yet-implemented
        checked: total number of checks performed
    """
    components = spec.get("components", {})
    if not components:
        return [], [], 0

    errors: list[tuple[str, str, str]] = []
    warnings: list[tuple[str, str, str, str]] = []
    checked = 0

    for comp_name, comp_def in components.items():
        all_elements = _resolve_component_elements(comp_name, components)
        excluded = _excluded_apps(comp_def)

        # Deduplicate while preserving order
        seen: set[str] = set()
        unique_elements: list[str] = []
        for e in all_elements:
            if e not in seen:
                seen.add(e)
                unique_elements.append(e)

        for elem_id in unique_elements:
            for app_name in APPS:
                checked += 1
                if find_id_in_app(elem_id, app_name):
                    continue
                if app_name in excluded:
                    warnings.append((app_name, comp_name, elem_id, excluded[app_name]))
                else:
                    errors.append((app_name, comp_name, elem_id))

    return errors, warnings, checked


def _gated_tab_ids(nav: dict) -> list[str]:
    """Collect the element IDs declared in navigation.gated_tabs.

    Each entry is either a mapping with an `id` key (the documented form) or a
    bare string. Anything without an id is skipped.
    """
    ids: list[str] = []
    for entry in nav.get("gated_tabs", []) or []:
        if isinstance(entry, dict) and entry.get("id"):
            ids.append(str(entry["id"]))
        elif isinstance(entry, str):
            ids.append(entry)
    return ids


def lint_navigation(spec: dict) -> tuple[list[tuple[str, str, str]], int]:
    """Check the navigation SKELETON across all 7 apps.

    Every `navigation.tabs` + `navigation.gated_tabs` id must be present in each
    client's source, MINUS the per-client absences declared in
    `navigation.nav_exceptions` ({tab-id: {client: reason}}). The skeleton is the
    part of the UI that priority #1 requires to be 100% uniform, so this is the
    one completeness check gated on the merge path (`--nav`, the
    `ui-nav-skeleton-lint` gate — the merge-gate scope table). The broad
    page/component completeness lint stays advisory (it has known per-client
    gaps).

    Returns (misses, checked) where misses is a list of (client, kind, tab_id)
    with kind in {"tab", "gated_tab"}.
    """
    nav = spec.get("navigation", {}) or {}
    exceptions = nav.get("nav_exceptions", {}) or {}
    entries: list[tuple[str, str]] = [("tab", t) for t in (nav.get("tabs", []) or [])]
    entries += [("gated_tab", t) for t in _gated_tab_ids(nav)]

    misses: list[tuple[str, str, str]] = []
    checked = 0
    for kind, tab_id in entries:
        exempt = exceptions.get(tab_id, {}) or {}
        # Route-derived tabs (feed-tab, …) may be built dynamically from a
        # `{route}-tab` template → loose match. Gated entries (admin-tab) are
        # explicit literals → strict match (loose would false-positive them).
        matcher = find_id_strict_in_app if kind == "gated_tab" else find_id_in_app
        for app_name in APPS:
            if app_name in exempt:
                continue
            checked += 1
            if not matcher(tab_id, app_name):
                misses.append((app_name, kind, tab_id))
    return misses, checked


def _run_nav_only(spec: dict) -> int:
    """`--nav` mode: lint ONLY the navigation skeleton and exit 1 on any miss.

    This is the `ui-nav-skeleton-lint` merge-gate entry point (the merge-gate
    scope table, added 2026-09-14 —
    `docs/goal/architecture/convention-gates.md` § *ui.yaml nav-skeleton gate*).
    The `.github/workflows/ci.yml` `ui-lint` job that used to invoke this flag
    has had no trigger since 2026-03-30 and is not a live backstop.
    """
    misses, checked = lint_navigation(spec)
    nav = spec.get("navigation", {}) or {}
    exceptions = nav.get("nav_exceptions", {}) or {}
    n_exc = sum(len(v) for v in exceptions.values() if isinstance(v, dict))

    if misses:
        by_client: dict[str, list[tuple[str, str]]] = {}
        for client, kind, tab_id in sorted(misses):
            by_client.setdefault(client, []).append((kind, tab_id))
        print(
            f"ui.yaml nav-skeleton lint: {len(misses)} missing nav entr(y/ies) "
            f"across {len(by_client)} app(s) ({checked} checks)\n"
        )
        for client in sorted(by_client):
            print(f"  {client}:")
            for kind, tab_id in by_client[client]:
                print(f"    {kind:10s} {tab_id}")
        print(
            "\n  A required nav tab / gated entry is absent on a client. Either add"
            "\n  the id to that client's nav surface, or — if it is genuinely absent"
            "\n  for a platform reason — declare it in navigation.nav_exceptions in"
            "\n  tests/e2e-unified/ui.yaml with a one-line reason + owner."
        )
        return 1

    print(
        f"ui.yaml nav-skeleton lint: OK — {checked} checks across {len(APPS)} "
        f"apps, {n_exc} declared exception(s)."
    )
    return 0


def main():
    verbose = "--verbose" in sys.argv or "-v" in sys.argv
    run_components = "--no-components" not in sys.argv

    spec = yaml.load(UI_YAML.read_text(), Loader=_YAML_LOADER)

    # `--nav`: lint ONLY the navigation skeleton and exit (merge-gated path,
    # `ui-nav-skeleton-lint`).
    if "--nav" in sys.argv:
        sys.exit(_run_nav_only(spec))

    missing = []
    checked = 0

    # Only lint pages listed in navigation.pages + search
    # Pages like admin-*, p2p, conflicts, moderation, nostr, profile are
    # client-specific and not required on all 7 clients.
    nav_pages = set(spec.get("navigation", {}).get("pages", []))
    nav_pages.add("search")  # search is a core page but not in nav.pages

    # Check page elements — the page's own `elements` plus every `sub_pages`
    # entry's `elements`. A sub-page (a modal / detail pane reached from the
    # page) is page scope like any other: until 2026-08-10 this loop read only
    # `elements`, so an id whose ONLY page scope was a sub-page was checked on
    # no app at all — which is how tui shipped its `event_detail` without
    # description, location or time while both UI lints stayed green.
    # A sub-page's `components:` list needs no walk here: lint_components()
    # below checks every component in the registry against every app already,
    # independently of which page uses it.
    for page_name, page_def in spec.get("pages", {}).items():
        if page_name not in nav_pages:
            continue
        # (element_id, report_label) in declaration order, deduplicated within
        # the page — a sub-page routinely repeats one of its parent's ids
        # (events.create_calendar re-lists calendar-name), and find_id_in_app
        # searches the whole app, so a second check would only echo the first.
        scoped: list[tuple[str, str]] = []
        seen_ids: set[str] = set()
        for elem_id in page_def.get("elements", []) or []:
            if elem_id not in seen_ids:
                seen_ids.add(elem_id)
                scoped.append((elem_id, page_name))
        for sub_name, sub_def in (page_def.get("sub_pages") or {}).items():
            for elem_id in (sub_def or {}).get("elements", []) or []:
                if elem_id not in seen_ids:
                    seen_ids.add(elem_id)
                    scoped.append((elem_id, f"{page_name}.{sub_name}"))

        # STRICT matching for page/sub-page elements (2026-08-10). A page element
        # is an explicit id placed as a literal on each app; the loose matcher's
        # dynamic-prefix inference (built for route-derived `{route}-tab` ids)
        # passes any id whose prefix has *some* template and whose tail appears
        # as a bare token anywhere — which was hiding three real gaps behind the
        # word "feed": tui's `feed-delete-button` + `bridge-form-bridge-select`
        # (absent from apps/fauna-tui/src entirely, present on the other six) and
        # android's `feed-factor-select` (named only in a KDoc block).
        # MEASURED before flipping: strict changes exactly those 3 page entries,
        # all three verified real by grep — no legitimate dynamic page element
        # exists today. COMPONENTS are deliberately NOT flipped: the same probe
        # says 48 component entries are loose-only, and most are genuinely
        # constructed (`inbox-mode-{m.value}`, `bridge-link-field-{key}`,
        # `admin-dns-domain-role-address-{role}-select`, `calendar-view-{mode}`)
        # — exactly what the dynamic-prefix inference exists for. Don't "finish
        # the job" by flipping lint_components() without re-measuring.
        # lint_navigation() keeps the loose matcher for `tabs` for the same reason.
        for elem_id, label in scoped:
            for app_name in APPS:
                checked += 1
                if not find_id_strict_in_app(elem_id, app_name):
                    missing.append((app_name, label, elem_id))

    # Check global elements
    for elem_id in spec.get("global", {}).get("elements", []):
        for app_name in APPS:
            checked += 1
            if not find_id_in_app(elem_id, app_name):
                missing.append((app_name, "global", elem_id))

    # Check navigation skeleton (tabs + gated_tabs, minus nav_exceptions).
    # Shares the single source of truth with the merge-gated `--nav` mode.
    nav_misses, nav_checked = lint_navigation(spec)
    checked += nav_checked
    for app_name, _kind, tab_id in nav_misses:
        missing.append((app_name, "navigation", tab_id))

    # --- Component validation ---
    comp_errors: list[tuple[str, str, str]] = []
    comp_warnings: list[tuple[str, str, str, str]] = []
    comp_checked = 0
    if run_components:
        comp_errors, comp_warnings, comp_checked = lint_components(spec, verbose)
        checked += comp_checked

    # --- Report: pages ---
    exit_code = 0

    if missing:
        by_client: dict[str, list[tuple[str, str]]] = {}
        for client, page, elem in sorted(missing):
            by_client.setdefault(client, []).append((page, elem))

        print(f"ui.yaml lint (pages): {len(missing)} missing element(s) across {len(by_client)} app(s)")
        print(f"  ({checked - comp_checked} page checks)\n")

        for client in sorted(by_client):
            items = by_client[client]
            print(f"  {client} ({len(items)} missing):")
            for page, elem in items:
                print(f"    {page:20s} {elem}")
            print()
        exit_code = 1

    # --- Report: components ---
    if run_components:
        if comp_errors:
            by_client_comp: dict[str, list[tuple[str, str]]] = {}
            for client, comp, elem in sorted(comp_errors):
                by_client_comp.setdefault(client, []).append((comp, elem))

            print(f"ui.yaml lint (components): {len(comp_errors)} missing element(s) across {len(by_client_comp)} app(s)")
            print(f"  ({comp_checked} component checks)\n")

            for client in sorted(by_client_comp):
                items = by_client_comp[client]
                print(f"  {client} ({len(items)} missing):")
                for comp, elem in items:
                    print(f"    {comp:30s} {elem}")
                print()
            exit_code = 1

        if comp_warnings:
            by_client_warn: dict[str, list[tuple[str, str]]] = {}
            for client, comp, elem, _reason in sorted(comp_warnings):
                by_client_warn.setdefault(client, []).append((comp, elem))

            print(f"ui.yaml lint (components): {len(comp_warnings)} not-yet-implemented (warnings)")
            for client in sorted(by_client_warn):
                items = by_client_warn[client]
                print(f"  {client} ({len(items)}):")
                for comp, elem in items:
                    print(f"    {comp:30s} {elem}")
            print()

        if not comp_errors and not comp_warnings:
            print(f"ui.yaml lint (components): all component elements present ({comp_checked} checks).")

    if not missing and not comp_errors:
        total_hard = checked - len(comp_warnings) if run_components else checked
        print(f"ui.yaml lint: all elements present in all 7 clients ({total_hard} checks, {len(comp_warnings)} warnings).")

    sys.exit(exit_code)


if __name__ == "__main__":
    main()
