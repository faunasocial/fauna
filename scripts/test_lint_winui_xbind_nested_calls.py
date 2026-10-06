"""Unit tests for scripts/lint_winui_xbind_nested_calls.py.

Run with: uv run pytest scripts/test_lint_winui_xbind_nested_calls.py -v

The lint encodes a documented WinUI build trap: the `x:Bind` compiler
cannot compile a function call whose
argument is itself a function call — `{x:Bind Foo(Bar(x))}`. On ARM64 it crashes
the out-of-process XamlCompiler.exe opaquely (microsoft/microsoft-ui-xaml#8871).
A *single* call with property/literal args is fine; only NESTED calls are the
trap. The fix is to extract a helper method and bind to that.
"""
from pathlib import Path
import sys

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import lint_winui_xbind_nested_calls as lint


def _count(xaml):
    return len(lint.find_nested_xbind_violations(xaml))


def test_nested_call_is_flagged():
    xaml = '<TextBlock Visibility="{x:Bind local:Foo.BoolToVis(Foo.IsReady(Item))}" />'
    v = lint.find_nested_xbind_violations(xaml)
    assert len(v) == 1


def test_single_call_with_property_arg_is_ok():
    xaml = '<TextBlock Visibility="{x:Bind local:FeedPage.BoolToVisibility(HasQuotedPost)}" />'
    assert _count(xaml) == 0


def test_single_call_with_dotted_property_path_arg_is_ok():
    # The real DnsConfigView shape: one call, a dotted property-path argument.
    xaml = '<Border Visibility="{x:Bind local:DnsConfigView.BoolToVisibility(ViewModel.TldPriceVisible), Mode=OneWay}" />'
    assert _count(xaml) == 0


def test_multi_arg_single_call_is_ok():
    xaml = '<TextBlock Text="{x:Bind local:FeedPage.FormatTagChip(Tags, 0)}" />'
    assert _count(xaml) == 0


def test_zero_arg_call_is_ok():
    xaml = '<Image helpers:ImageHashBind.Loader="{x:Bind local:FeedPage.GetImageLoader(), Mode=OneTime}" />'
    assert _count(xaml) == 0


def test_plain_property_path_is_ok():
    xaml = '<TextBlock Text="{x:Bind ViewModel.ContactFormVisible}" />'
    assert _count(xaml) == 0


def test_empty_xbind_is_ok():
    xaml = '<Grid DataContext="{x:Bind}" />'
    assert _count(xaml) == 0


def test_nested_call_among_other_args_is_flagged():
    xaml = '<TextBlock Text="{x:Bind local:Foo.Fmt(Tags, local:Foo.Inner(Item))}" />'
    assert _count(xaml) == 1


def test_nested_zero_arg_call_is_flagged():
    xaml = '<Image Source="{x:Bind local:Foo.Pick(local:Foo.Default())}" />'
    assert _count(xaml) == 1


def test_binding_settings_after_call_are_ignored():
    # Settings (Mode/Converter/FallbackValue) live after a top-level comma and
    # carry no call-parens — must not trip the scan.
    xaml = '<TextBlock Visibility="{x:Bind local:Foo.ToVis(IsOn), Mode=OneWay, FallbackValue=Collapsed}" />'
    assert _count(xaml) == 0


def test_nested_markup_extension_converter_is_ok():
    xaml = '<TextBlock Text="{x:Bind local:Foo.Fmt(Value), Converter={StaticResource trim}}" />'
    assert _count(xaml) == 0


def test_attached_property_path_paren_is_not_a_call():
    # x:Bind attached-property path uses parens but it is not a function call —
    # the '(' is not preceded by a method identifier, so it must not be flagged.
    xaml = '<TextBlock Text="{x:Bind (Grid.Row)}" />'
    assert _count(xaml) == 0


def test_string_literal_arg_with_parens_is_ok():
    # Parens inside a quoted string literal argument are not nested calls.
    xaml = "<TextBlock Text=\"{x:Bind local:Foo.Echo('a(b)c')}\" />"
    assert _count(xaml) == 0


def test_only_xbind_is_checked_not_plain_binding():
    # {Binding} is a different markup extension (runtime-evaluated, no XAML
    # compiler) — even a nested-looking expression there is out of scope.
    xaml = '<TextBlock Text="{Binding Foo(Bar(x))}" />'
    assert _count(xaml) == 0


def test_multiple_xbinds_one_nested():
    xaml = (
        '<Grid>'
        '  <TextBlock Text="{x:Bind local:Foo.Ok(Value)}" />'
        '  <TextBlock Text="{x:Bind local:Foo.Bad(local:Foo.Inner(Value))}" />'
        '</Grid>'
    )
    assert _count(xaml) == 1


def test_line_number_points_at_xbind():
    xaml = "\n".join([
        "<Grid>",                                                       # line 1
        '    <TextBlock Text="{x:Bind local:Foo.A(local:Foo.B(X))}" />',  # line 2
        "</Grid>",                                                      # line 3
    ])
    v = lint.find_nested_xbind_violations(xaml)
    assert len(v) == 1 and v[0].line == 2


def test_multiline_xbind_nested_call_is_flagged():
    xaml = (
        '<TextBlock\n'
        '    Visibility="{x:Bind\n'
        '        local:Foo.ToVis(local:Foo.IsReady(Item))}" />'
    )
    assert _count(xaml) == 1


def test_real_winui_tree_passes():
    # No nested-call x:Bind exists in the tree today; the lint must stay green so
    # it acts purely as a forward guard against re-introduction.
    violations = lint.scan_repo()
    assert violations == [], "\n".join(
        f"{v.source}:{v.line} {v.expression}" for v in violations
    )
