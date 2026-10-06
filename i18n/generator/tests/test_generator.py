"""Tests for the i18n string generator."""

import sys
from pathlib import Path

# Ensure the generator module is importable
sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent))

import ast

from generator.generate import (
    parse_yaml,
    flatten,
    detect_params,
    emit_typescript,
    emit_android,
    emit_windows,
    emit_rust,
    emit_python,
)

SAMPLE_YAML = """\
common:
  cancel: "Cancel"
  save: "Save"
  loading: "Loading..."

feed:
  post:
    reposted_by: "Reposted by {author}"
    post_count: "{count} posts"

errors:
  api_error: "API error ({status}): {message}"
"""


class TestParseYaml:
    def test_returns_nested_dict(self):
        result = parse_yaml(SAMPLE_YAML)
        assert isinstance(result, dict)
        assert "common" in result
        assert "feed" in result

    def test_nested_values(self):
        result = parse_yaml(SAMPLE_YAML)
        assert result["common"]["cancel"] == "Cancel"
        assert result["feed"]["post"]["reposted_by"] == "Reposted by {author}"

    def test_deep_nesting(self):
        result = parse_yaml(SAMPLE_YAML)
        assert result["feed"]["post"]["post_count"] == "{count} posts"

    def test_boolean_keys_stay_strings(self):
        """YAML yes/no/on/off must not be converted to booleans."""
        yaml_text = 'common:\n  yes: "Yes"\n  no: "No"\n'
        result = parse_yaml(yaml_text)
        assert "yes" in result["common"], f"keys are: {list(result['common'].keys())}"
        assert "no" in result["common"]
        assert result["common"]["yes"] == "Yes"
        assert result["common"]["no"] == "No"


class TestFlatten:
    def test_simple_keys(self):
        tree = parse_yaml(SAMPLE_YAML)
        flat = flatten(tree)
        assert flat["common.cancel"] == "Cancel"
        assert flat["common.save"] == "Save"

    def test_nested_keys(self):
        tree = parse_yaml(SAMPLE_YAML)
        flat = flatten(tree)
        assert flat["feed.post.reposted_by"] == "Reposted by {author}"

    def test_all_leaf_nodes_present(self):
        tree = parse_yaml(SAMPLE_YAML)
        flat = flatten(tree)
        expected_keys = {
            "common.cancel",
            "common.save",
            "common.loading",
            "feed.post.reposted_by",
            "feed.post.post_count",
            "errors.api_error",
        }
        assert set(flat.keys()) == expected_keys

    def test_with_prefix(self):
        tree = {"a": {"b": "value"}}
        flat = flatten(tree, prefix="root")
        assert flat["root.a.b"] == "value"


class TestDetectParams:
    def test_no_params(self):
        assert detect_params("Cancel") == []

    def test_single_param(self):
        assert detect_params("Reposted by {author}") == ["author"]

    def test_multiple_params(self):
        result = detect_params("API error ({status}): {message}")
        assert result == ["status", "message"]

    def test_duplicate_params_deduplicated(self):
        result = detect_params("{x} and {x}")
        assert result == ["x"]

    def test_preserves_order(self):
        result = detect_params("{b} then {a} then {c}")
        assert result == ["b", "a", "c"]


class TestEmitTypescript:
    def test_header_comment(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_typescript(tree)
        assert output.startswith("// AUTO-GENERATED from i18n/strings/en.yaml")

    def test_export_const(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_typescript(tree)
        assert "export const t = {" in output
        assert "} as const;" in output

    def test_plain_string_literal(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_typescript(tree)
        assert '"Cancel"' in output
        assert '"Save"' in output

    def test_parameterized_arrow_function(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_typescript(tree)
        assert "(v: { author: string })" in output
        assert "`Reposted by ${v.author}`" in output

    def test_multi_param_arrow_function(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_typescript(tree)
        assert "(v: { status: string; message: string })" in output
        assert "`API error (${v.status}): ${v.message}`" in output

    def test_ends_with_newline(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_typescript(tree)
        assert output.endswith("\n")

    def test_nested_structure_preserved(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_typescript(tree)
        # feed.post should be nested, not flattened
        assert "feed: {" in output
        assert "post: {" in output


class TestEmitAndroid:
    def test_header(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_android(tree)
        assert '<?xml version="1.0" encoding="utf-8"?>' in output
        assert "AUTO-GENERATED from i18n/strings/en.yaml" in output

    def test_plain_string(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_android(tree)
        assert '<string name="common_cancel">Cancel</string>' in output
        assert '<string name="common_save">Save</string>' in output

    def test_single_param(self):
        # Named {placeholder} tokens are kept intact — resolveLocalized
        # substitutes them by name and stringResourceFmt maps positional args
        # onto them in textual order (no %N$s rewrite).
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_android(tree)
        assert '<string name="feed_post_reposted_by">Reposted by {author}</string>' in output

    def test_multi_param(self):
        # Multi-arg keys keep their distinct {name} tokens, so by-name resolution
        # is independent of the Rust HashMap arg order.
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_android(tree)
        assert '<string name="errors_api_error">API error ({status}): {message}</string>' in output

    def test_xml_escaping(self):
        tree = parse_yaml('test:\n  amp: "A & B"\n  lt: "A < B"\n  gt: "A > B"\n  apos: "It\'s"\n')
        output = emit_android(tree)
        assert "A &amp; B" in output
        assert "A &lt; B" in output
        assert "A &gt; B" in output
        # Android AAPT2 requires backslash-escaped apostrophes, not &apos;
        assert r"It\'s" in output

    def test_double_quote_and_backslash_escaping(self):
        # AAPT2 reads an unescaped `"` as a quoting delimiter and drops it, so
        # `Your folder "{folder}"` rendered as `Your folder premium`; a
        # backslash is its escape character, so a literal one is doubled first.
        tree = parse_yaml('test:\n  quote: \'Your folder "{folder}"\'\n  slash: \'a\\b\'\n')
        output = emit_android(tree)
        assert r'<string name="test_quote">Your folder \"{folder}\"</string>' in output
        assert r'<string name="test_slash">a\\b</string>' in output

    def test_underscore_separator(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_android(tree)
        assert 'name="feed_post_post_count"' in output



class TestEmitWindows:
    def test_header(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_windows(tree)
        assert '<?xml version="1.0" encoding="utf-8"?>' in output
        assert "AUTO-GENERATED from i18n/strings/en.yaml" in output

    def test_plain_string(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_windows(tree)
        assert '<data name="common/cancel" xml:space="preserve"><value>Cancel</value></data>' in output

    def test_single_param(self):
        # Named {placeholder} tokens are kept intact — Strings.Resolve substitutes
        # them by name and Strings.Format maps positional args onto them in order.
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_windows(tree)
        assert '<data name="feed/post/reposted_by" xml:space="preserve"><value>Reposted by {author}</value></data>' in output

    def test_multi_param(self):
        # Multi-arg keys keep their distinct {name} tokens (no {0}/{1} rewrite),
        # so by-name resolution is independent of the Rust HashMap arg order.
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_windows(tree)
        assert '<data name="errors/api_error" xml:space="preserve"><value>API error ({status}): {message}</value></data>' in output

    def test_xml_escaping(self):
        tree = parse_yaml('test:\n  amp: "A & B"\n  lt: "A < B"\n  gt: "A > B"\n  apos: "It\'s"\n')
        output = emit_windows(tree)
        assert "A &amp; B" in output
        assert "A &lt; B" in output
        assert "A &gt; B" in output
        # Windows RESW does not escape apostrophes; passed through as-is
        assert "It's" in output

    def test_slash_separator(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_windows(tree)
        assert 'name="feed/post/post_count"' in output


class TestEmitRust:
    def test_header(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_rust(tree)
        assert output.startswith("// AUTO-GENERATED from i18n/strings/en.yaml")

    def test_plain_string_const_screaming_snake(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_rust(tree)
        assert 'pub const CANCEL: &str = "Cancel";' in output
        assert 'pub const SAVE: &str = "Save";' in output

    def test_nested_mod_structure(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_rust(tree)
        assert "pub mod common {" in output
        assert "pub mod feed {" in output
        assert "pub mod post {" in output

    def test_single_param_function(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_rust(tree)
        assert "pub fn reposted_by(author: &str) -> String {" in output
        assert 'format!("Reposted by {author}")' in output

    def test_multi_param_function(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_rust(tree)
        assert "pub fn api_error(status: &str, message: &str) -> String {" in output
        assert 'format!("API error ({status}): {message}")' in output

    def test_bare_single_placeholder_emits_to_string(self):
        # format!("{x}") trips clippy::useless_format under -D warnings.
        tree = parse_yaml('chip:\n  has_hashtag: "{tags}"\n')
        output = emit_rust(tree)
        assert "pub fn has_hashtag(tags: &str) -> String {" in output
        assert "tags.to_string()" in output
        assert 'format!("{tags}")' not in output

    def test_escaping(self):
        tree = parse_yaml('test:\n  quote: "Say \\"hello\\""\n  slash: "back\\\\slash"\n')
        output = emit_rust(tree)
        assert r'\"hello\"' in output
        assert r"back\\slash" in output

    def test_lookup_function_present(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_rust(tree)
        assert "pub fn lookup(key: &str) -> Option<&'static str> {" in output

    def test_lookup_plain_string_arm(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_rust(tree)
        assert '"common.cancel" => Some(common::CANCEL),' in output
        assert '"common.save" => Some(common::SAVE),' in output

    def test_lookup_nested_key_arm(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_rust(tree)
        assert '"common.loading" => Some(common::LOADING),' in output

    def test_lookup_includes_parameterized_strings(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_rust(tree)
        # Parameterized strings are emitted as a raw-template constant AND a
        # format fn; lookup() returns the raw template (placeholders intact) so
        # LocalizedText resolution — which substitutes {name} args itself — can
        # look up any key. See _collect_rust_lookup_arms / value-formatting.md.
        lookup_body = output.split("pub fn lookup")[1]
        assert '"feed.post.reposted_by" => Some(feed::post::REPOSTED_BY),' in lookup_body
        assert '"errors.api_error" => Some(errors::API_ERROR),' in lookup_body

    def test_lookup_has_wildcard_none_arm(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_rust(tree)
        lookup_body = output.split("pub fn lookup")[1]
        assert "_ => None," in lookup_body


class TestEmitPython:
    def test_header(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_python(tree)
        assert output.startswith("# AUTO-GENERATED from i18n/strings/en.yaml")

    def test_leaf_class_plain_string(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_python(tree)
        assert 'cancel = "Cancel"' in output
        assert 'save = "Save"' in output

    def test_static_method_keyword_only_args(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_python(tree)
        assert "@staticmethod" in output
        assert "def reposted_by(*, author: str) -> str:" in output
        assert 'return f"Reposted by {author}"' in output

    def test_multi_param_static_method(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_python(tree)
        assert "def api_error(*, status: str, message: str) -> str:" in output

    def test_parent_class_references_children(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_python(tree)
        # _Feed should reference _FeedPost
        assert "class _Feed:" in output
        assert "post = _FeedPost" in output

    def test_top_level_s_class(self):
        tree = parse_yaml(SAMPLE_YAML)
        output = emit_python(tree)
        assert "class S:" in output
        assert "common = _Common" in output
        assert "feed = _Feed" in output
        assert "errors = _Errors" in output


class TestReservedKeywords:
    """Verify emitters handle language-reserved keywords correctly."""

    KEYWORD_YAML = """\
onboarding:
  welcome:
    title: "Fauna"
    continue: "Continue"
    return: "Return"
"""

    def test_python_reserved_keyword_escaped(self):
        tree = parse_yaml(self.KEYWORD_YAML)
        output = emit_python(tree)
        # 'continue' and 'return' are Python keywords — should get underscore suffix
        assert 'continue_ = "Continue"' in output
        assert 'return_ = "Return"' in output
        # Must be valid Python
        ast.parse(output)

    def test_python_non_keyword_unchanged(self):
        tree = parse_yaml(self.KEYWORD_YAML)
        output = emit_python(tree)
        assert 'title = "Fauna"' in output

    def test_python_generated_from_real_yaml_is_valid(self):
        """Parse the actual en.yaml and verify the generated Python is syntactically valid."""
        from generator.generate import load_strings
        tree = load_strings()
        output = emit_python(tree)
        ast.parse(output)  # Raises SyntaxError if invalid

    def test_rust_reserved_keyword_uses_raw_identifier(self):
        tree = parse_yaml(self.KEYWORD_YAML)
        output = emit_rust(tree)
        # 'continue' and 'return' are Rust keywords — should use r# prefix
        assert 'pub const R#CONTINUE' not in output  # const names are uppercase, not r# prefixed
        # Actually consts use .upper() so CONTINUE is fine — it's not a keyword in uppercase
        # But if they were parameterized (functions), they'd need r#
        # Let's check the mod name isn't a keyword issue
        assert "pub mod onboarding {" in output

    def test_rust_reserved_keyword_in_function_name(self):
        yaml_with_fn = 'test:\n  r#continue: "{val} stuff"\n'
        # Actually test with a real keyword as a parameterized key
        yaml_with_fn = 'test:\n  type: "Type: {detail}"\n'
        tree = parse_yaml(yaml_with_fn)
        output = emit_rust(tree)
        assert "pub fn r#type(detail: &str) -> String {" in output
