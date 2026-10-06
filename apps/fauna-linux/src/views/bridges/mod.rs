pub mod detail;
pub mod list;

pub use list::{BridgeListHandles, update_bridge_list};

use adw::prelude::*;
use fauna_client_bridges::bridges_ui::BridgeStatus;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::client::FaunaClient;
use crate::i18n::strings::bridges as bridges_strings;

/// Snapshot of the most-recent `fauna.bridges.list` reply, keyed by bridge id,
/// retained so the detail pane can render the metadata-driven link form from
/// each provider's `link_modes`. Populated by the `BridgesLoaded` handler.
pub type BridgesSnapshot = Rc<RefCell<HashMap<String, BridgeStatus>>>;

/// Handles for the bridges view returned to `app.rs`.
pub struct BridgesViewHandles {
    pub list_handles: BridgeListHandles,
    pub split: adw::NavigationSplitView,
    /// `(bridge_id, follows_list)` of the currently-open detail pane, if any —
    /// so `app.rs`'s `DataMessage::BridgeFollowsLoaded` handler can repaint the
    /// right widget when the reply for the open bridge lands.
    /// Updated on every row activation below; read-only from `app.rs`'s side.
    pub open_detail: Rc<RefCell<Option<(String, gtk::ListBox)>>>,
}

/// Build the full bridges view: NavigationSplitView with bridge list on the
/// left and bridge detail on the right.
pub fn build_bridges_view(
    client: &Rc<FaunaClient>,
    bridges_snapshot: BridgesSnapshot,
) -> (adw::NavigationSplitView, BridgesViewHandles) {
    let split = adw::NavigationSplitView::new();

    let (list_widget, list_handles) = list::build_bridge_list(client);

    let list_page = adw::NavigationPage::builder()
        .title(bridges_strings::TITLE)
        .child(&list_widget)
        .build();

    // Default empty detail page.
    let empty = adw::StatusPage::builder()
        .title(bridges_strings::SELECT_BRIDGE)
        .description(bridges_strings::SELECT_BRIDGE_DESC)
        .icon_name("network-transmit-symbolic")
        .build();

    let detail_page = adw::NavigationPage::builder()
        .title(bridges_strings::DETAIL_TITLE)
        .child(&empty)
        .build();

    split.set_sidebar(Some(&list_page));
    split.set_content(Some(&detail_page));

    let open_detail: Rc<RefCell<Option<(String, gtk::ListBox)>>> = Rc::new(RefCell::new(None));

    // Wire list row selection to open the detail pane. This is the single
    // activation path: the detail pane's link form is rendered from the
    // selected bridge's `link_modes`, read from the retained snapshot.
    {
        let sp = split.clone();
        let c = Rc::clone(client);
        let snapshot = Rc::clone(&bridges_snapshot);
        let open_detail = Rc::clone(&open_detail);
        list_handles.list_box.connect_row_activated(move |_, row| {
            let bridge_id = row.widget_name().to_string();
            if bridge_id.is_empty() {
                return;
            }
            // Derive a display name from the row's label child.
            let bridge_name = extract_bridge_name(row);
            let (link_modes, linked, error, settings) = snapshot
                .borrow()
                .get(&bridge_id)
                .map(|b| {
                    (
                        b.link_modes.clone().unwrap_or_default(),
                        b.linked,
                        b.error.clone(),
                        b.settings.clone(),
                    )
                })
                .unwrap_or_default();
            let (detail, handles) = detail::build_bridge_detail(
                &bridge_id,
                &bridge_name,
                link_modes,
                linked,
                error,
                settings,
                Rc::clone(&c),
            );
            let page = adw::NavigationPage::builder()
                .title(&bridge_name)
                .child(&detail)
                .build();
            sp.set_content(Some(&page));
            *open_detail.borrow_mut() = Some((bridge_id.clone(), handles.follows_list));
            c.fetch_bridge_follows(&bridge_id);
        });
    }

    let handles = BridgesViewHandles {
        list_handles,
        split: split.clone(),
        open_detail,
    };
    (split, handles)
}

/// Extract the bridge name from a list row widget.
/// Row structure: ListBoxRow → HBox → VBox → Label(name), Label(status)
fn extract_bridge_name(row: &gtk::ListBoxRow) -> String {
    use gtk::prelude::WidgetExt;
    row.child()
        .and_then(|hbox| hbox.first_child())
        .and_then(|vbox| vbox.first_child())
        .and_then(|w| w.downcast::<gtk::Label>().ok())
        .map(|label| label.text().to_string())
        .unwrap_or_else(|| row.widget_name().to_string())
}
