"""Unit tests for scripts/lint_winui_xbind_datatemplate_scope.py.

Run with: uv run pytest scripts/test_lint_winui_xbind_datatemplate_scope.py -v

The lint encodes a documented WinUI trap: an `{x:Bind ...}` expression inside a
`<DataTemplate>` that declares no `x:DataType` has no binding context. It silently
resolves to nothing, or — when it is the template's only `x:Bind` — crashes the
generated `SetDataRoot` outright. Measured twice (RecipientPicker.xaml,
NotificationsPage.xaml) before this lint existed, plus 70 further sites.
"""
from pathlib import Path
import sys

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import lint_winui_xbind_datatemplate_scope as lint


def _count(xaml):
    return len(lint.find_unscoped_xbind_violations(xaml))


def test_xbind_in_unannotated_datatemplate_is_flagged():
    xaml = """
    <DataTemplate>
        <TextBlock AutomationProperties.AutomationId="{x:Bind ids:Ids.Foo}" />
    </DataTemplate>
    """
    v = lint.find_unscoped_xbind_violations(xaml)
    assert len(v) == 1
    assert v[0].attribute == "AutomationProperties.AutomationId"
    assert v[0].element == "TextBlock"


def test_xbind_in_annotated_datatemplate_is_ok():
    xaml = """
    <DataTemplate x:DataType="local:ThreadRow">
        <TextBlock AutomationProperties.AutomationId="{x:Bind ids:Ids.Foo}" />
    </DataTemplate>
    """
    assert _count(xaml) == 0


def test_page_scope_xbind_outside_any_datatemplate_is_ok():
    xaml = '<Page><Button AutomationProperties.AutomationId="{x:Bind ids:Ids.Foo}" /></Page>'
    assert _count(xaml) == 0


def test_classic_binding_in_unannotated_template_is_ok():
    # {Binding} is runtime-evaluated; it needs no x:DataType and is not this lint's concern.
    xaml = """
    <DataTemplate>
        <TextBlock Text="{Binding Name}" />
    </DataTemplate>
    """
    assert _count(xaml) == 0


def test_nested_datatemplate_does_not_inherit_parent_datatype():
    # Each DataTemplate is its own binding scope; the outer x:DataType does not
    # cover the inner, unannotated template.
    xaml = """
    <DataTemplate x:DataType="local:Outer">
        <ItemsControl>
            <ItemsControl.ItemTemplate>
                <DataTemplate>
                    <TextBlock AutomationProperties.AutomationId="{x:Bind ids:Ids.Inner}" />
                </DataTemplate>
            </ItemsControl.ItemTemplate>
        </ItemsControl>
    </DataTemplate>
    """
    v = lint.find_unscoped_xbind_violations(xaml)
    assert len(v) == 1
    assert v[0].expression.startswith("{x:Bind ids:Ids.Inner")


def test_self_closing_datatemplate_is_ok():
    xaml = '<ItemsControl.ItemTemplate><DataTemplate /></ItemsControl.ItemTemplate>'
    assert _count(xaml) == 0


def test_datatemplate_resources_property_element_is_not_confused_with_datatemplate():
    xaml = """
    <DataTemplate x:DataType="local:Row">
        <DataTemplate.Resources>
            <x:String x:Key="Foo">bar</x:String>
        </DataTemplate.Resources>
        <TextBlock AutomationProperties.AutomationId="{x:Bind ids:Ids.Foo}" />
    </DataTemplate>
    """
    assert _count(xaml) == 0


def test_after_closing_datatemplate_scope_reverts_to_outer():
    xaml = """
    <StackPanel>
        <ItemsControl.ItemTemplate>
            <DataTemplate>
                <TextBlock AutomationProperties.AutomationId="{x:Bind ids:Ids.Inner}" />
            </DataTemplate>
        </ItemsControl.ItemTemplate>
        <Button AutomationProperties.AutomationId="{x:Bind ids:Ids.PageScope}" />
    </StackPanel>
    """
    v = lint.find_unscoped_xbind_violations(xaml)
    assert len(v) == 1
    assert "Inner" in v[0].expression
