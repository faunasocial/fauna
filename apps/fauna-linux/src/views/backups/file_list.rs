use fauna_ui_ids as ids;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::client::FaunaClient;
use crate::i18n::strings::backups as backups_strings;

/// Build the file list pane for a selected snapshot.
/// Returns (outer_box, list_box, header_bar). The check-integrity
/// button moved to the list pane (snapshot_list.rs) because the
/// server endpoint is folder-scoped, not snapshot-scoped.
pub fn build_snapshot_file_list(
    snapshot_timestamp: &str,
) -> (gtk::Box, gtk::ListBox, adw::HeaderBar) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some(snapshot_timestamp))));

    outer.append(&header);

    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::None);
    list_box.add_css_class("boxed-list");
    crate::testid::set_test_id(&list_box, ids::SNAPSHOT_DETAIL_FILES);

    let placeholder = gtk::Label::new(Some(crate::i18n::strings::backups::NO_FILES_IN_SNAPSHOT));
    placeholder.add_css_class("dim-label");
    placeholder.set_margin_top(16);
    placeholder.set_margin_bottom(16);
    list_box.set_placeholder(Some(&placeholder));

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&list_box)
        .build();

    outer.append(&scrolled);
    (outer, list_box, header)
}

/// Build a file row within a snapshot: path + size + a per-file download
/// button (`snapshot-file-download-button`, indexed — `backup-restore.md`
/// § 3). The button renders for every row, not just `"regular"` files — the
/// linux twin of windows' unfiltered `SnapshotFileInfo` list — and a
/// directory/symlink click fails closed with an `error-message` from
/// [`FaunaClient::save_snapshot_file`] rather than being hidden client-side.
pub fn build_backup_file_row(
    file: &fauna_backups_machine::SnapshotFileRow,
    size: &str,
    client: &Rc<FaunaClient>,
) -> gtk::ListBoxRow {
    let path_label = gtk::Label::new(Some(&file.path));
    path_label.set_halign(gtk::Align::Start);
    path_label.set_hexpand(true);
    path_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);

    let size_label = gtk::Label::new(Some(size));
    size_label.set_halign(gtk::Align::End);
    size_label.add_css_class("dim-label");
    size_label.add_css_class("caption");

    let download_btn = gtk::Button::new();
    download_btn.set_icon_name("folder-download-symbolic");
    download_btn.set_tooltip_text(Some(backups_strings::DOWNLOAD));
    download_btn.add_css_class("flat");
    crate::testid::set_test_id(&download_btn, ids::SNAPSHOT_FILE_DOWNLOAD_BUTTON);
    {
        let c = Rc::clone(client);
        let file = file.clone();
        download_btn.connect_clicked(move |btn| {
            let suggested_name = file
                .path
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or(&file.path)
                .to_string();

            // Under e2e automation, bypass the native picker entirely and
            // write straight to the harness-provided dir — mirrors windows'
            // `FAUNA_E2E_DOWNLOAD_DIR` / `DirectorySnapshotFileSaver` split
            // (`BackupsPage.xaml.cs` `OnNavigatedTo`).
            //
            // Two gates, both required (`conversations::host::manager()` for the
            // same shape): the `#[cfg]` is convention 15's outer boundary and
            // `e2e_mode_enabled()` the inner switch. The predicate ALONE was
            // sound here — its production twin returns literal `false`, so the
            // branch folded and the string never reached the binary, measured —
            // but soundness that only an optimizer can see is not a compile
            // gate, and no text scanner can read it either.
            #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
            if crate::e2e_mode_enabled()
                && let Ok(dir) = std::env::var("FAUNA_E2E_DOWNLOAD_DIR")
            {
                let save_path = std::path::Path::new(&dir).join(&suggested_name);
                c.save_snapshot_file(file.clone(), &save_path.display().to_string());
                return;
            }

            let parent = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let dialog = gtk::FileDialog::builder()
                .title(backups_strings::DOWNLOAD)
                .initial_name(suggested_name.as_str())
                .build();
            let c2 = Rc::clone(&c);
            let file2 = file.clone();
            dialog.save(parent.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(gfile) = result
                    && let Some(path) = gfile.path()
                {
                    c2.save_snapshot_file(file2.clone(), &path.display().to_string());
                }
            });
        });
    }

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(6);
    hbox.set_margin_bottom(6);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);
    hbox.append(&path_label);
    hbox.append(&size_label);
    hbox.append(&download_btn);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    row
}

/// Populate the snapshot file list from the machine's opened detail — one row
/// per `SnapshotFileRow` (`path` + human-readable `size_bytes` + the download
/// button). `None` (no row open) empties the list, which is what makes the
/// machine's two closure rules visible: selecting another set closes the
/// detail, and so does a re-read that no longer lists the open snapshot (else
/// its download buttons would point at dropped manifests).
pub fn populate_snapshot_file_list(
    list_box: &gtk::ListBox,
    detail: Option<&fauna_backups_machine::SnapshotDetail>,
    client: &Rc<FaunaClient>,
) {
    // Remove all existing rows.
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    let Some(detail) = detail else {
        return;
    };
    for file in &detail.files {
        let size = crate::i18n::byte_size(file.size_bytes.max(0) as u64);
        let row = build_backup_file_row(file, &size, client);
        list_box.append(&row);
    }
}
