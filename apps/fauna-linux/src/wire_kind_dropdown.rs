//! A value-carrying `gtk::DropDown`: the model strings are the wire values a
//! `select(id, value)` e2e contract drives, a `set_expression` closure re-maps
//! each to its painted label, and [`WireKindDropdown::selected_kind`] /
//! [`WireKindDropdown::select_kind`] read/write the wire value directly
//! rather than a display index.
//!
//! `views/backups/destinations.rs`'s `KindSelect` and
//! `views/personalization/publish_sheet.rs`'s `PublishKindSelect` hand-rolled
//! this byte-identically (bar the catalog, label function, test id and
//! fallback constant) before this lift — found by the same-crate arm of the
//! dev-fleet near-duplicate-function scanner. Several *other* views build a
//! superficially similar value/label `DropDown` split (`admin.rs`'s
//! `build_tier_select_dropdown`, `devices_folders/folders.rs`'s per-item
//! destination-attach picker) but each of those is driven by dynamic runtime
//! data rather than a static option catalog and carries its own live-update
//! wiring — genuinely different shapes, not this one.

use crate::testid::set_test_id;

/// See the module docs.
#[derive(Clone)]
pub(crate) struct WireKindDropdown {
    pub(crate) dd: gtk::DropDown,
    /// Wire values, index-aligned with the `DropDown`'s string model.
    values: Vec<String>,
}

impl WireKindDropdown {
    /// `values` are the wire discriminators in catalog/display order; `label`
    /// renders a value's painted text (typically an i18n lookup).
    pub(crate) fn build(
        values: Vec<String>,
        test_id: &str,
        label: impl Fn(&str) -> String + Send + Sync + 'static,
    ) -> Self {
        let value_refs: Vec<&str> = values.iter().map(String::as_str).collect();
        let dd = gtk::DropDown::builder()
            .model(&gtk::StringList::new(&value_refs))
            .build();
        let label_expr = gtk::ClosureExpression::new::<String>(
            &[] as &[gtk::Expression],
            gtk::glib::closure!(move |item: gtk::StringObject| label(item.string().as_str())),
        );
        dd.set_expression(Some(&label_expr));
        set_test_id(&dd, test_id);
        Self { dd, values }
    }

    /// The catalog's wire values, in display order — what the model was built
    /// from, for a test asserting the picker offers exactly the shared
    /// catalog.
    #[cfg(test)]
    pub(crate) fn values(&self) -> &[String] {
        &self.values
    }

    /// The wire discriminator currently selected. Falls back to the catalog's
    /// first entry, then to `default`, if the selection is somehow out of
    /// range or the catalog is empty.
    pub(crate) fn selected_kind<'a>(&'a self, default: &'a str) -> &'a str {
        self.values
            .get(self.dd.selected() as usize)
            .or_else(|| self.values.first())
            .map(String::as_str)
            .unwrap_or(default)
    }

    /// Preselect `wire`. An unrecognised kind leaves the selection alone.
    pub(crate) fn select_kind(&self, wire: &str) {
        if let Some(i) = self.values.iter().position(|v| v == wire) {
            self.dd.set_selected(i as u32);
        }
    }
}
