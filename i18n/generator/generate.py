# /// script
# requires-python = ">=3.10"
# dependencies = ["pyyaml"]
# ///
"""i18n string generator — reads en.yaml and emits per-platform files.

Usage:
    python generate.py           # generate all targets
    python generate.py --check   # verify generated files are up-to-date

The PEP-723 header above lets `uv run i18n/generator/generate.py` resolve
pyyaml automatically on machines that don't have it system-wide (e.g. fresh
macOS dev hosts). The justfile recipe relies on this.
"""

from __future__ import annotations

import argparse
import keyword
import re
import sys
from pathlib import Path
from typing import Callable

import yaml

# Repo root is two levels up from this file's directory (i18n/generator/ -> repo root)
REPO_ROOT = Path(__file__).resolve().parent.parent.parent
STRINGS_FILE = REPO_ROOT / "i18n" / "strings" / "en.yaml"

# ---------------------------------------------------------------------------
# Target registry
# ---------------------------------------------------------------------------

# Each target is (output_path, emitter_function)
_targets: list[tuple[Path, Callable[[dict], str]]] = []


def register_target(rel_path: str, emitter: Callable[[dict], str]) -> None:
    """Register an output target. rel_path is relative to REPO_ROOT."""
    _targets.append((REPO_ROOT / rel_path, emitter))


# ---------------------------------------------------------------------------
# Core functions
# ---------------------------------------------------------------------------


class _StringKeyLoader(yaml.SafeLoader):
    """YAML loader that keeps boolean-like keys (yes/no/on/off) as strings."""
    pass


# Override the boolean resolver so that bare yes/no/on/off/true/false stay as
# strings.  We want ALL scalars that PyYAML would convert to bool to remain
# strings, because our keys and values are always meant to be text.
_StringKeyLoader.add_constructor(
    "tag:yaml.org,2002:bool",
    lambda loader, node: loader.construct_scalar(node),
)


def parse_yaml(text: str) -> dict:
    """Parse a YAML string into a nested dict."""
    return yaml.load(text, Loader=_StringKeyLoader)


def load_strings() -> dict:
    """Load strings from the canonical en.yaml file."""
    return parse_yaml(STRINGS_FILE.read_text(encoding="utf-8"))


def flatten(tree: dict, prefix: str = "") -> dict[str, str]:
    """Flatten a nested dict into dot-separated keys."""
    result: dict[str, str] = {}
    for key, value in tree.items():
        full_key = f"{prefix}.{key}" if prefix else key
        if isinstance(value, dict):
            result.update(flatten(value, full_key))
        else:
            result[full_key] = str(value)
    return result


def detect_params(text: str) -> list[str]:
    """Find {name} interpolation parameters in a string, preserving order, deduplicating."""
    seen: set[str] = set()
    result: list[str] = []
    for match in re.finditer(r"\{(\w+)\}", text):
        name = match.group(1)
        if name not in seen:
            seen.add(name)
            result.append(name)
    return result


# ---------------------------------------------------------------------------
# TypeScript emitter
# ---------------------------------------------------------------------------


def _ts_value(text: str, indent: int) -> str:
    """Convert a string value to a TS literal or arrow function."""
    params = detect_params(text)
    if not params:
        # Escape backslashes and double quotes in plain strings
        escaped = text.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")
        return f'"{escaped}"'
    # Build arrow function with typed parameter object
    type_fields = "; ".join(f"{p}: string" for p in params)
    # Convert {name} to ${v.name} for template literal
    template = re.sub(r"\{(\w+)\}", r"${v.\1}", text)
    return f"(v: {{ {type_fields} }}) => `{template}`"


def _emit_ts_object(tree: dict, indent: int = 2) -> str:
    """Recursively emit a TS object literal."""
    lines: list[str] = []
    pad = " " * indent
    items = list(tree.items())
    for key, value in items:
        if isinstance(value, dict):
            lines.append(f"{pad}{key}: {{")
            lines.append(_emit_ts_object(value, indent + 2))
            lines.append(f"{pad}}},")
        else:
            ts_val = _ts_value(str(value), indent)
            lines.append(f"{pad}{key}: {ts_val},")
    return "\n".join(lines)


def emit_typescript(tree: dict) -> str:
    """Emit a TypeScript const object from a nested string dict."""
    header = "// AUTO-GENERATED from i18n/strings/en.yaml — do not edit"
    body = _emit_ts_object(tree)
    return f"{header}\nexport const t = {{\n{body}\n}} as const;\n"


# ---------------------------------------------------------------------------
# Android emitter
# ---------------------------------------------------------------------------


def _xml_escape(text: str) -> str:
    """Escape XML special characters."""
    text = text.replace("&", "&amp;")
    text = text.replace("<", "&lt;")
    text = text.replace(">", "&gt;")
    return text


def _android_value(text: str) -> str:
    """XML-escape + AAPT2 apostrophe-escape; keep {name} placeholders intact
    (no positional %N$s rewrite).

    Android resolves LocalizedText by NAME — the canonical `ui/util/Localized.kt`
    `resolveLocalized` substitutes each `{name}` from the args map, and the
    `stringResourceFmt` helper maps a direct caller's positional args onto the
    `{name}` tokens in textual order. This mirrors `_windows_value` (and web
    L(), Apple Bundle, Linux fauna_i18n): the old `{name}`->`%N$s` rewrite forced
    multi-arg LocalizedText keys to depend on the Rust HashMap iteration order
    (non-deterministic), which would misorder them (e.g. `mail_aliases.hits_with_last`
    = "{count} hits · last {date}" could render "Jun 1 hits · last 3"). Keeping
    the named tokens makes by-name resolution order-independent. See
    docs/goal/behavior/value-formatting.md.
    """
    # AAPT2's escape character first, so the escapes below are not doubled.
    result = _xml_escape(text.replace("\\", "\\\\"))
    # Android AAPT2 requires apostrophes escaped with backslash, not &apos;
    result = result.replace("'", "\\'")
    # An unescaped double quote is an AAPT2 quoting delimiter and is dropped.
    result = result.replace('"', '\\"')
    return result


def emit_android(tree: dict) -> str:
    """Emit Android strings.xml from a nested string dict."""
    flat = flatten(tree)
    lines: list[str] = []
    lines.append('<?xml version="1.0" encoding="utf-8"?>')
    lines.append("<!-- AUTO-GENERATED from i18n/strings/en.yaml \u2014 do not edit -->")
    lines.append("<resources>")
    for key, value in flat.items():
        android_key = key.replace(".", "_")
        android_val = _android_value(value)
        lines.append(f'    <string name="{android_key}">{android_val}</string>')
    lines.append("</resources>\n")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Windows emitter
# ---------------------------------------------------------------------------


def _windows_value(text: str) -> str:
    """XML-escape; keep {name} placeholders intact (no positional rewrite).

    Windows resolves LocalizedText by NAME (Strings.Resolve substitutes
    `{name}` from the args map) — the same named convention every other app
    uses (web L(), Apple Bundle, Android getString, Linux fauna_i18n). The old
    `{name}`->`{N}` rewrite forced multi-arg LocalizedText keys to depend on the
    Rust HashMap iteration order (which is non-deterministic), misordering them
    (e.g. `time.uptime_dhm` rendered "0d 0h 1m"). Strings.Format maps its
    positional args onto these `{name}` tokens in textual order, so it keeps
    working without positional placeholders. See
    docs/goal/behavior/value-formatting.md.
    """
    return _xml_escape(text)


def emit_windows(tree: dict) -> str:
    """Emit WinUI .resw XML from a nested string dict."""
    flat = flatten(tree)
    lines: list[str] = []
    lines.append('<?xml version="1.0" encoding="utf-8"?>')
    lines.append("<!-- AUTO-GENERATED from i18n/strings/en.yaml \u2014 do not edit -->")
    lines.append("<root>")
    for key, value in flat.items():
        win_key = key.replace(".", "/")
        win_val = _windows_value(value)
        lines.append(f'    <data name="{win_key}" xml:space="preserve"><value>{win_val}</value></data>')
    lines.append("</root>\n")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Rust emitter
# ---------------------------------------------------------------------------


_RUST_KEYWORDS = frozenset([
    "as", "break", "const", "continue", "crate", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
    "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
    "super", "trait", "true", "type", "unsafe", "use", "where", "while",
    "async", "await", "dyn", "abstract", "become", "box", "do", "final",
    "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
])


def _rust_safe_name(name: str) -> str:
    """Use raw identifier syntax if name is a Rust keyword."""
    if name in _RUST_KEYWORDS:
        return f"r#{name}"
    return name


def _rust_escape(text: str) -> str:
    """Escape backslashes, double quotes, and newlines for Rust string literals."""
    return text.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")


def _emit_rust_mod(tree: dict, indent: int = 0) -> str:
    """Recursively emit nested Rust mod blocks."""
    lines: list[str] = []
    pad = "    " * indent
    items = list(tree.items())
    for i, (key, value) in enumerate(items):
        if i > 0:
            lines.append("")
        if isinstance(value, dict):
            safe_mod = _rust_safe_name(key)
            lines.append(f"{pad}pub mod {safe_mod} {{")
            lines.append(_emit_rust_mod(value, indent + 1))
            lines.append(f"{pad}}}")
        else:
            text = str(value)
            params = detect_params(text)
            const_name = key.upper()
            escaped = _rust_escape(text)
            # Always emit the raw template as a constant so `lookup()` can
            # return it for runtime placeholder substitution by callers like
            # `LocalizedText` resolution.
            lines.append(f'{pad}pub const {const_name}: &str = "{escaped}";')
            if params:
                safe_fn = _rust_safe_name(key)
                param_list = ", ".join(f"{p}: &str" for p in params)
                fmt_str = escaped
                lines.append(f"{pad}pub fn {safe_fn}({param_list}) -> String {{")
                if len(params) == 1 and fmt_str == f"{{{params[0]}}}":
                    # A bare single-placeholder template: format!("{x}") trips
                    # clippy::useless_format under the workspace's -D warnings.
                    lines.append(f"{pad}    {params[0]}.to_string()")
                else:
                    # {name} already works as Rust named format args
                    lines.append(f'{pad}    format!("{fmt_str}")')
                lines.append(f"{pad}}}")
    return "\n".join(lines)


def _collect_rust_lookup_arms(tree: dict, prefix: str = "") -> list[tuple[str, str]]:
    """Walk the tree and return (dotted_key, rust_const_path) pairs for every leaf.

    Both non-parameterized strings (constants like `FOO`) and parameterized
    strings (also emitted as `FOO` template constants alongside the
    `foo(args)` format function) are included, so callers like
    `LocalizedText` resolution that need the raw template — placeholders
    intact for runtime arg substitution — can look up any key.
    """
    arms: list[tuple[str, str]] = []
    for key, value in tree.items():
        full_key = f"{prefix}.{key}" if prefix else key
        if isinstance(value, dict):
            arms.extend(_collect_rust_lookup_arms(value, full_key))
        else:
            # Build the Rust path: "foo.bar.baz" -> "foo::bar::BAZ"
            parts = full_key.split(".")
            mod_parts = [_rust_safe_name(p) for p in parts[:-1]]
            const_name = parts[-1].upper()
            rust_path = "::".join(mod_parts + [const_name])
            arms.append((full_key, rust_path))
    return arms


def _emit_rust_lookup(tree: dict) -> str:
    """Emit a `pub fn lookup(key: &str) -> Option<&'static str>` function.

    Returns the raw template string for every key (placeholders like
    `{name}` left intact). Callers performing runtime substitution
    (`LocalizedText.{key, args}`) substitute placeholders themselves.
    """
    arms = _collect_rust_lookup_arms(tree)
    lines: list[str] = []
    lines.append("pub fn lookup(key: &str) -> Option<&'static str> {")
    lines.append("    match key {")
    for dotted_key, rust_path in arms:
        lines.append(f'        "{dotted_key}" => Some({rust_path}),')
    lines.append("        _ => None,")
    lines.append("    }")
    lines.append("}")
    return "\n".join(lines)


def emit_rust(tree: dict) -> str:
    """Emit a Rust module with nested pub mod blocks plus a lookup function."""
    header = "// AUTO-GENERATED from i18n/strings/en.yaml — do not edit"
    body = _emit_rust_mod(tree)
    lookup = _emit_rust_lookup(tree)
    return f"{header}\n\n{body}\n\n{lookup}\n"


# ---------------------------------------------------------------------------
# Python emitter
# ---------------------------------------------------------------------------


def _py_escape(text: str) -> str:
    """Escape backslashes, double quotes, and newlines for Python string literals."""
    return text.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")


def _py_safe_name(name: str) -> str:
    """Append underscore if name is a Python reserved keyword."""
    if keyword.iskeyword(name):
        return name + "_"
    return name


def _py_class_name(parts: list[str]) -> str:
    """Build a PascalCase class name from path parts, prefixed with _."""
    return "_" + "".join(p.capitalize() for p in parts)


def _emit_python_classes(tree: dict) -> str:
    """Emit Python classes for dot-access pattern."""
    classes: list[str] = []

    def _collect(node: dict, path: list[str]) -> str:
        """Process a node. Returns the class name for this node."""
        # Check if this node has any dict children (branches)
        branches: list[tuple[str, dict]] = []
        leaves: list[tuple[str, str]] = []
        for key, value in node.items():
            if isinstance(value, dict):
                branches.append((key, value))
            else:
                leaves.append((key, str(value)))

        # Process child branches first (depth-first)
        child_class_names: dict[str, str] = {}
        for key, subtree in branches:
            child_class_names[key] = _collect(subtree, path + [key])

        # Build this class
        if path:
            class_name = _py_class_name(path)
        else:
            class_name = "S"

        lines: list[str] = []
        lines.append(f"class {class_name}:")

        # Handle empty classes (e.g. from `key: {}` in YAML)
        if not leaves and not branches:
            lines.append("    pass")

        # Emit leaf attributes
        for key, text in leaves:
            safe_key = _py_safe_name(key)
            params = detect_params(text)
            if not params:
                escaped = _py_escape(text)
                lines.append(f'    {safe_key} = "{escaped}"')
            else:
                param_list = ", ".join(f"{p}: str" for p in params)
                escaped = _py_escape(text)
                # Convert {name} to Python f-string {name}
                lines.append("    @staticmethod")
                lines.append(f"    def {safe_key}(*, {param_list}) -> str:")
                lines.append(f'        return f"{escaped}"')

        # Emit branch references
        for key, _subtree in branches:
            safe_key = _py_safe_name(key)
            lines.append(f"    {safe_key} = {child_class_names[key]}")

        # Empty classes need a pass statement
        if not leaves and not branches:
            lines.append("    pass")

        classes.append("\n".join(lines))
        return class_name

    _collect(tree, [])
    return "\n\n\n".join(classes)


def emit_python(tree: dict) -> str:
    """Emit Python classes for dot-access string lookup."""
    header = "# AUTO-GENERATED from i18n/strings/en.yaml — do not edit"
    body = _emit_python_classes(tree)
    return f"{header}\n\n\n{body}\n"


# ---------------------------------------------------------------------------
# Swift emitter
# ---------------------------------------------------------------------------


_SWIFT_KEYWORDS = frozenset([
    "as", "break", "case", "catch", "class", "continue", "default", "defer",
    "deinit", "do", "else", "enum", "extension", "fallthrough", "false",
    "fileprivate", "for", "func", "guard", "if", "import", "in", "init",
    "inout", "internal", "is", "let", "nil", "open", "operator", "override",
    "precedencegroup", "private", "protocol", "public", "repeat", "rethrows",
    "return", "self", "Self", "static", "struct", "subscript", "super",
    "switch", "throw", "throws", "true", "try", "typealias", "var", "where",
    "while",
    # Context-sensitive keywords commonly used as identifiers
    "associatedtype", "convenience", "dynamic", "didSet", "final", "get",
    "indirect", "lazy", "mutating", "nonmutating", "optional", "postfix",
    "prefix", "required", "set", "some", "type", "unowned", "weak", "willSet",
])


def _snake_to_camel(name: str) -> str:
    """Convert snake_case to camelCase."""
    parts = name.split("_")
    if len(parts) <= 1:
        return name
    return parts[0] + "".join(p.capitalize() for p in parts[1:])


def _swift_safe_name(name: str) -> str:
    """Backtick-escape Swift keywords."""
    camel = _snake_to_camel(name)
    if camel in _SWIFT_KEYWORDS:
        return f"`{camel}`"
    return camel


def _swift_escape(text: str) -> str:
    """Escape backslashes, double quotes, and newlines for Swift string literals."""
    return text.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")


def _emit_swift_enum(tree: dict, indent: int = 1) -> str:
    """Recursively emit nested Swift enum blocks."""
    lines: list[str] = []
    pad = "    " * indent
    items = list(tree.items())
    for i, (key, value) in enumerate(items):
        if i > 0:
            lines.append("")
        if isinstance(value, dict):
            safe_name = _swift_safe_name(key)
            lines.append(f"{pad}public enum {safe_name} {{")
            lines.append(_emit_swift_enum(value, indent + 1))
            lines.append(f"{pad}}}")
        else:
            text = str(value)
            params = detect_params(text)
            if not params:
                safe_name = _swift_safe_name(key)
                escaped = _swift_escape(text)
                lines.append(f'{pad}public static let {safe_name} = "{escaped}"')
            else:
                safe_name = _swift_safe_name(key)
                param_list = ", ".join(
                    f"{_snake_to_camel(p)}: String" for p in params
                )
                # Convert {name} to \(name) for Swift string interpolation
                interpolated = _swift_escape(text)
                for p in params:
                    interpolated = interpolated.replace(
                        "{" + p + "}", "\\(" + _snake_to_camel(p) + ")"
                    )
                lines.append(f"{pad}public static func {safe_name}({param_list}) -> String {{")
                lines.append(f'{pad}    "{interpolated}"')
                lines.append(f"{pad}}}")
    return "\n".join(lines)


def _flatten_for_lookup(tree: dict, prefix: str = "", include_params: bool = False) -> list[tuple[str, str]]:
    """Walk the tree and yield (dotted_key, value) for parameterless leaves only.

    Parameterised entries (those with `{name}` placeholders) are skipped — runtime
    callers that need a static template can render them themselves; the lookup
    table is for keys that resolve to a finished string (provider display names,
    field labels, help text, etc., where the caller has only the key string).
    """
    out: list[tuple[str, str]] = []
    for key, value in tree.items():
        full = f"{prefix}.{key}" if prefix else key
        if isinstance(value, dict):
            out.extend(_flatten_for_lookup(value, full, include_params))
        elif isinstance(value, str):
            if include_params or not detect_params(value):
                # When include_params is True, emit the template verbatim
                # (e.g. "Hello {name}.") so callers that look up by key
                # can do `{placeholder}` substitution from a runtime args
                # dict — required by the cross-client `LocalizedText`
                # pattern in the OnboardingMachine snapshots.
                out.append((full, value))
    return out


def emit_swift(tree: dict) -> str:
    """Emit a Swift file with nested enums for type-safe string access, plus a
    runtime `L.lookup(_ key: String) -> String` helper backed by a flat
    dictionary.

    The runtime helper is needed for cases where the call site only has the
    dotted key as a String (e.g. `ProviderMeta.displayNameKey` /
    `FieldMeta.labelKey` from the provider registry). Falls back to returning
    the key itself when missing, so unknown keys are visible in the UI rather
    than silently producing empty strings.

    `lookup` lives inside `enum L` (a static func) rather than as a free
    `L(_:)` function — Swift forbids a type and a function sharing a name in
    the same scope.
    """
    header_lines = [
        "// AUTO-GENERATED from i18n/strings/en.yaml — do not edit",
        "// swiftlint:disable all",
        "// swiftformat:disable all",
        "",
        "public enum L {",
    ]
    body = _emit_swift_enum(tree)

    # Flat dictionary + lookup function. Keys are dot-joined paths. We
    # include parameterized templates verbatim so `LocalizedText`-bearing
    # snapshots (handle_check, invite_request) can be rendered by a
    # client-side `{placeholder}` substitution layer; clients that want
    # the typed form can keep using the nested L.x.y(arg:) functions.
    flat_entries = _flatten_for_lookup(tree, include_params=True)
    flat_entries.sort(key=lambda kv: kv[0])
    lookup_lines = [
        "",
        "    /// Runtime lookup of an i18n string by dotted key (e.g.",
        "    /// \"provisioning.cloudflare.name\"). Used when the call site only",
        "    /// has the key as a String, e.g. `ProviderMeta.displayNameKey`.",
        "    /// Returns the key itself when missing — surfaces typos in the UI",
        "    /// rather than silently producing empty strings.",
        "    public static func lookup(_ key: String) -> String {",
        "        _LFlat.table[key] ?? key",
        "    }",
    ]
    flat_lines = [
        "",
        "private enum _LFlat {",
        "    static let table: [String: String] = [",
    ]
    for k, v in flat_entries:
        flat_lines.append(f'        "{_swift_escape(k)}": "{_swift_escape(v)}",')
    flat_lines.append("    ]")
    flat_lines.append("}")

    return (
        "\n".join(header_lines)
        + "\n"
        + body
        + "\n"
        + "\n".join(lookup_lines)
        + "\n}\n"
        + "\n".join(flat_lines)
        + "\n"
    )


# ---------------------------------------------------------------------------
# Generation engine
# ---------------------------------------------------------------------------


def generate_all(check: bool = False) -> bool:
    """Run all registered targets. Returns True if all OK."""
    tree = load_strings()
    all_ok = True
    for output_path, emitter in _targets:
        content = emitter(tree)
        if check:
            if not output_path.exists():
                print(f"MISSING: {output_path.relative_to(REPO_ROOT)}")
                all_ok = False
            elif output_path.read_text(encoding="utf-8") != content:
                print(f"OUT OF DATE: {output_path.relative_to(REPO_ROOT)}")
                all_ok = False
            else:
                print(f"OK: {output_path.relative_to(REPO_ROOT)}")
        else:
            output_path.parent.mkdir(parents=True, exist_ok=True)
            if output_path.exists() and output_path.read_text(encoding="utf-8") == content:
                print(f"UNCHANGED: {output_path.relative_to(REPO_ROOT)}")
            else:
                output_path.write_text(content, encoding="utf-8")
                print(f"WROTE: {output_path.relative_to(REPO_ROOT)}")
    return all_ok


# ---------------------------------------------------------------------------
# Register targets
# ---------------------------------------------------------------------------

register_target("apps/fauna-web/src/lib/i18n/strings.ts", emit_typescript)
register_target("apps/fauna-android/app/src/main/res/values/i18n_strings.xml", emit_android)
register_target("apps/fauna-windows/FaunaApp/FaunaApp/Strings/en-US/Resources.resw", emit_windows)
# One Rust emission, not two: both Rust apps (linux + tui) consume the shared
# `fauna-i18n` crate. linux used to register its own `emit_rust` target here and
# the two files were byte-identical; it now re-exports this one as
# `crate::i18n::strings` (`apps/fauna-linux/src/i18n/mod.rs`).
register_target("libs/fauna-i18n/src/strings.rs", emit_rust)
register_target("tests/e2e-unified/i18n/strings.py", emit_python)
register_target("apps/fauna-apple/FaunaKit/Sources/FaunaExtensionKit/Generated/L.swift", emit_swift)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main() -> None:
    parser = argparse.ArgumentParser(description="Generate i18n string files")
    parser.add_argument(
        "--check",
        action="store_true",
        help="Verify generated files are up-to-date (exit 1 if not)",
    )
    args = parser.parse_args()
    ok = generate_all(check=args.check)
    if not ok:
        sys.exit(1)


if __name__ == "__main__":
    main()
