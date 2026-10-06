"""Unit tests for scripts/lint_winui_datatemplate_names.py.

Run with: uv run pytest scripts/test_lint_winui_datatemplate_names.py -v

The lint encodes the documented WinUI list-count trap: a
DataTemplate whose root is a bare layout container (Grid/StackPanel/Border/…)
carrying AutomationProperties.AutomationId is pruned from the UIA tree unless it
also carries AutomationProperties.Name, so FlaUI FindAllDescendants(ByAutomationId)
counts 0 rows even when rows exist. Content-control roots (TextBlock, Button, …)
have native peers and need no Name.
"""
from pathlib import Path
import sys

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import lint_winui_datatemplate_names as lint


def _ids(violations):
    return [v.automation_id for v in violations]


def test_bare_grid_root_with_id_no_name_is_flagged():
    xaml = """
        <DataTemplate x:DataType="models:EventInfo">
            <Grid AutomationProperties.AutomationId="event-card" Padding="12">
                <TextBlock Text="{x:Bind Summary}" />
            </Grid>
        </DataTemplate>
    """
    v = lint.find_datatemplate_root_violations(xaml)
    assert _ids(v) == ["event-card"]
    assert v[0].element == "Grid"


def test_grid_root_with_name_binding_is_ok():
    xaml = """
        <DataTemplate x:DataType="models:EventInfo">
            <Grid AutomationProperties.AutomationId="event-card"
                  AutomationProperties.Name="{x:Bind Summary}" Padding="12">
                <TextBlock Text="{x:Bind Summary}" />
            </Grid>
        </DataTemplate>
    """
    assert lint.find_datatemplate_root_violations(xaml) == []


def test_border_root_with_id_no_name_is_flagged():
    xaml = """
        <DataTemplate>
            <Border AutomationProperties.AutomationId="bridge-card" Padding="4">
                <TextBlock Text="{Binding}" />
            </Border>
        </DataTemplate>
    """
    assert _ids(lint.find_datatemplate_root_violations(xaml)) == ["bridge-card"]


def test_textblock_root_with_id_no_name_is_ok():
    # Content controls have native UIA peers — no Name needed (calendar-item,
    # recipient-picker-suggestion in the real tree).
    xaml = """
        <DataTemplate x:DataType="models:CalendarInfo">
            <TextBlock Text="{x:Bind Name}"
                       AutomationProperties.AutomationId="calendar-item" />
        </DataTemplate>
    """
    assert lint.find_datatemplate_root_violations(xaml) == []


def test_name_with_single_space_is_ok():
    # provisioning-step-row uses a literal Name=" " — any non-empty value un-prunes.
    xaml = """
        <DataTemplate>
            <Grid AutomationProperties.AutomationId="provisioning-step-row"
                  AutomationProperties.Name=" ">
                <TextBlock Text="step" />
            </Grid>
        </DataTemplate>
    """
    assert lint.find_datatemplate_root_violations(xaml) == []


def test_empty_name_string_is_flagged():
    # An empty Name="" does not un-prune (UIA treats it as no name).
    xaml = """
        <DataTemplate>
            <Grid AutomationProperties.AutomationId="snapshot-item"
                  AutomationProperties.Name="">
                <TextBlock Text="x" />
            </Grid>
        </DataTemplate>
    """
    assert _ids(lint.find_datatemplate_root_violations(xaml)) == ["snapshot-item"]


def test_leading_comment_before_root_is_skipped():
    # RecipientPicker chip template: a comment precedes the real root.
    xaml = """
        <DataTemplate>
            <!-- AutomationId on the inner TextBlock, not the Border -->
            <Border Padding="4">
                <TextBlock AutomationProperties.AutomationId="recipient-picker-chip"
                           Text="{Binding Display}" />
            </Border>
        </DataTemplate>
    """
    # Root Border carries no AutomationId → not the trap; inner TextBlock is a
    # content control. No violation.
    assert lint.find_datatemplate_root_violations(xaml) == []


def test_root_without_automation_id_is_ignored():
    xaml = """
        <DataTemplate>
            <Grid Padding="4">
                <TextBlock Text="{Binding}" />
            </Grid>
        </DataTemplate>
    """
    assert lint.find_datatemplate_root_violations(xaml) == []


def test_custom_prefixed_control_root_is_ok():
    # A custom UserControl (controls:Foo) has its own AutomationPeer; not a bare
    # layout panel, so the rule doesn't apply.
    xaml = """
        <DataTemplate>
            <controls:FilterCard AutomationProperties.AutomationId="filter-item" />
        </DataTemplate>
    """
    assert lint.find_datatemplate_root_violations(xaml) == []


def test_only_root_is_checked_inner_layout_ignored():
    # Scope is the DataTemplate ROOT (the element list virtualization materializes
    # and FlaUI counts). A bare layout element NESTED inside is out of scope.
    xaml = """
        <DataTemplate>
            <TextBlock AutomationProperties.AutomationId="row" Text="x">
                <Grid AutomationProperties.AutomationId="inner" />
            </TextBlock>
        </DataTemplate>
    """
    assert lint.find_datatemplate_root_violations(xaml) == []


def test_multiple_datatemplates_in_one_file():
    xaml = """
        <DataTemplate>
            <Grid AutomationProperties.AutomationId="good"
                  AutomationProperties.Name="{x:Bind A}" />
        </DataTemplate>
        <DataTemplate>
            <StackPanel AutomationProperties.AutomationId="bad" />
        </DataTemplate>
    """
    assert _ids(lint.find_datatemplate_root_violations(xaml)) == ["bad"]


def test_line_number_points_at_root_element():
    xaml = "\n".join([
        "<DataTemplate>",                                          # line 1
        "    <Grid AutomationProperties.AutomationId=\"x\" />",    # line 2
        "</DataTemplate>",                                         # line 3
    ])
    v = lint.find_datatemplate_root_violations(xaml)
    assert len(v) == 1 and v[0].line == 2


def test_real_winui_tree_passes():
    # The 2026-05-25 sweep fixed all 18 bare-layout roots; the real tree must stay clean.
    violations = lint.scan_repo()
    assert violations == [], "\n".join(
        f"{v.source}:{v.line} <{v.element} id={v.automation_id}>" for v in violations
    )


# --- Scope 2: page-level containers (atproto-depth-selector was invisible
# to scope 1 because it's not a DataTemplate root) -------------------------------

def test_page_level_container_with_ui_yaml_id_and_no_name_is_flagged():
    xaml = """
        <Page>
            <StackPanel AutomationProperties.AutomationId="atproto-depth-selector">
                <RadioButton />
            </StackPanel>
        </Page>
    """
    v = lint.find_page_level_container_violations(xaml, frozenset({"atproto-depth-selector"}))
    assert _ids(v) == ["atproto-depth-selector"]
    assert v[0].element == "StackPanel"
    assert v[0].context == "page-level container"


def test_page_level_container_with_name_is_ok():
    xaml = """
        <Page>
            <StackPanel AutomationProperties.AutomationId="atproto-depth-selector"
                        AutomationProperties.Name="depth selector">
                <RadioButton />
            </StackPanel>
        </Page>
    """
    assert lint.find_page_level_container_violations(xaml, frozenset({"atproto-depth-selector"})) == []


def test_bare_itemscontrol_is_flagged_the_critical_alerts_regression():
    """An `ItemsControl` is a layout container for this trap's purposes.

    Regression for the FIFTH recurrence, and the one this lint watched go by:
    `critical-alerts` (MainPage.xaml) was a page-level ItemsControl with only an
    AutomationId, UIA pruned it while its `critical-alert` rows resolved
    normally, and windows' custody alarm therefore read as "never fires" across
    two sessions when it had been firing the whole time. The
    lint passed on that tree because `ItemsControl` was absent from
    LAYOUT_ROOT_TYPES — so the asymmetry, not the lint, is what finally named it.
    """
    xaml = """
        <Page>
            <ItemsControl AutomationProperties.AutomationId="critical-alerts">
                <ItemsControl.ItemTemplate />
            </ItemsControl>
        </Page>
    """
    v = lint.find_page_level_container_violations(xaml, frozenset({"critical-alerts"}))
    assert _ids(v) == ["critical-alerts"]
    assert v[0].element == "ItemsControl"
    # ...and naming it is the fix, exactly as for the panels.
    named = xaml.replace(
        'AutomationProperties.AutomationId="critical-alerts"',
        'AutomationProperties.AutomationId="critical-alerts"\n'
        '                          AutomationProperties.Name="critical alerts"',
    )
    assert lint.find_page_level_container_violations(named, frozenset({"critical-alerts"})) == []


def test_page_level_container_with_id_not_in_ui_yaml_is_not_flagged():
    # A decorative layout panel whose AutomationId ui.yaml never heard of — the
    # id-match requirement is what keeps this scope from flagging every page Grid.
    xaml = """
        <Page>
            <Grid AutomationProperties.AutomationId="internal-only-wrapper" />
        </Page>
    """
    assert lint.find_page_level_container_violations(xaml, frozenset({"atproto-depth-selector"})) == []


def test_page_level_container_inside_datatemplate_is_not_flagged():
    # Scope 1's job, not scope 2's — even though this id is ui.yaml-known.
    xaml = """
        <Page>
            <ListView>
                <ListView.ItemTemplate>
                    <DataTemplate>
                        <Grid AutomationProperties.AutomationId="atproto-depth-selector" />
                    </DataTemplate>
                </ListView.ItemTemplate>
            </ListView>
        </Page>
    """
    assert lint.find_page_level_container_violations(xaml, frozenset({"atproto-depth-selector"})) == []


def test_page_level_container_after_datatemplate_closes_is_still_checked():
    # The dt_depth counter must return to 0 once </DataTemplate> closes, not stay
    # "stuck inside" for the rest of the file.
    xaml = """
        <Page>
            <ListView>
                <ListView.ItemTemplate>
                    <DataTemplate>
                        <Grid AutomationProperties.AutomationId="row-item" />
                    </DataTemplate>
                </ListView.ItemTemplate>
            </ListView>
            <StackPanel AutomationProperties.AutomationId="atproto-depth-selector" />
        </Page>
    """
    v = lint.find_page_level_container_violations(xaml, frozenset({"atproto-depth-selector", "row-item"}))
    assert _ids(v) == ["atproto-depth-selector"]


def test_property_element_syntax_is_not_confused_with_a_real_element():
    # <Grid.RowDefinitions> must not be treated as a <Grid> instance (it would be,
    # under a tag-name regex that truncates at the first '.').
    xaml = """
        <Page>
            <Grid AutomationProperties.AutomationId="atproto-depth-selector"
                  AutomationProperties.Name="ok">
                <Grid.RowDefinitions>
                    <RowDefinition Height="Auto" AutomationProperties.AutomationId="atproto-depth-selector" />
                </Grid.RowDefinitions>
            </Grid>
        </Page>
    """
    assert lint.find_page_level_container_violations(xaml, frozenset({"atproto-depth-selector"})) == []


def test_code_behind_set_name_suppresses_the_violation():
    # DmMessageBubble.xaml's RenderLinkPreview/RenderQuote/RenderContentLabel pattern:
    # Name assigned imperatively via AutomationProperties.SetName, keyed off x:Name.
    xaml = """
        <Page>
            <Border x:Name="LinkPreviewCard"
                    AutomationProperties.AutomationId="link-preview-card"
                    Visibility="Collapsed" />
        </Page>
    """
    code_behind = """
        private void RenderLinkPreview() {
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(LinkPreviewCard, card.Title);
        }
    """
    v = lint.find_page_level_container_violations(
        xaml, frozenset({"link-preview-card"}), code_behind_text=code_behind
    )
    assert v == []


def test_x_name_without_matching_code_behind_set_name_is_still_flagged():
    # x:Name alone doesn't exempt an element — only a SetName call keyed on THAT
    # x:Name does (BackupsPage.xaml's DestinationModal/DestinationRemoveModal: real
    # SetName calls exist in that file, but target unrelated loop-local variables).
    xaml = """
        <Page>
            <StackPanel x:Name="DestinationModal"
                        AutomationProperties.AutomationId="backup-destination-add-modal" />
        </Page>
    """
    code_behind = """
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, d.Label);
    """
    v = lint.find_page_level_container_violations(
        xaml, frozenset({"backup-destination-add-modal"}), code_behind_text=code_behind
    )
    assert _ids(v) == ["backup-destination-add-modal"]


def test_load_ui_yaml_ids_includes_a_page_only_id():
    # atproto-depth-selector lives only in pages.atproto.elements at the time this
    # was written — missing from the flat `elements:` registry (a separate,
    # pre-existing ui.yaml drift). The real check must not depend on that registry
    # entry existing.
    ids = lint.load_ui_yaml_ids()
    assert "atproto-depth-selector" in ids
    assert "page-heading" in ids  # a registry-only global id, for the other half


def test_real_winui_tree_passes_page_level_scope():
    violations = lint.scan_repo_page_level(lint.load_ui_yaml_ids())
    assert violations == [], "\n".join(
        f"{v.source}:{v.line} <{v.element} id={v.automation_id}>" for v in violations
    )
