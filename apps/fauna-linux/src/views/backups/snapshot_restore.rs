//! The whole-snapshot restore panel (`ui/backups.md` § Whole-snapshot restore),
//! shown inside the open snapshot detail beside `snapshot-detail-files`: the
//! typed destination (`snapshot-restore-destination-input`, filled by an
//! untagged *Choose…* folder picker — the `folder-location-path-input` +
//! browse-button precedent, so the e2e test types into the entry), the button
//! (`snapshot-restore-button`, insensitive with no destination and while a
//! restore runs), then the progress line while it runs or the outcome line
//! after. Both lines exist only for their phase, so a wait on
//! `snapshot-restore-result` waits for a finished run.

use fauna_ui_ids as ids;
use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::client::FaunaClient;
use crate::i18n::strings::backups as bs;

pub struct SnapshotRestorePanel {
    pub widget: gtk::Box,
    dest: adw::EntryRow,
    button: gtk::Button,
    progress: gtk::Label,
    result: gtk::Label,
    /// The snapshot the panel is for; a different (or no) open snapshot resets it.
    snapshot_id: Cell<Option<i64>>,
    running: Cell<bool>,
}

impl SnapshotRestorePanel {
    pub fn build(client: &Rc<FaunaClient>) -> Rc<Self> {
        let dest = adw::EntryRow::builder()
            .title(bs::SNAPSHOT_RESTORE_DESTINATION_PLACEHOLDER)
            .build();
        crate::testid::set_test_id(&dest, ids::SNAPSHOT_RESTORE_DESTINATION_INPUT);
        let choose = gtk::Button::builder()
            .icon_name("folder-open-symbolic")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        dest.add_suffix(&choose);

        let button = gtk::Button::with_label(bs::SNAPSHOT_RESTORE_BUTTON);
        button.set_halign(gtk::Align::Start);
        button.set_sensitive(false);
        crate::testid::set_test_id(&button, ids::SNAPSHOT_RESTORE_BUTTON);

        let line = |id: &str| {
            let label = gtk::Label::new(None);
            label.set_halign(gtk::Align::Start);
            label.set_wrap(true);
            label.set_xalign(0.0);
            label.set_visible(false);
            crate::testid::set_test_id(&label, id);
            label
        };
        let progress = line(ids::SNAPSHOT_RESTORE_PROGRESS);
        let result = line(ids::SNAPSHOT_RESTORE_RESULT);

        let group = adw::PreferencesGroup::new();
        group.add(&dest);
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 8);
        widget.set_margin_top(8);
        widget.set_margin_bottom(8);
        widget.set_margin_start(12);
        widget.set_margin_end(12);
        widget.append(&group);
        widget.append(&button);
        widget.append(&progress);
        widget.append(&result);
        widget.set_visible(false);

        let panel = Rc::new(Self {
            widget,
            dest: dest.clone(),
            button: button.clone(),
            progress,
            result,
            snapshot_id: Cell::new(None),
            running: Cell::new(false),
        });

        {
            let panel = Rc::clone(&panel);
            dest.connect_changed(move |_| panel.sync_button());
        }
        // Choose… → fill the typed entry (the picker is not e2e-driveable).
        {
            let dest = dest.clone();
            choose.connect_clicked(move |btn| {
                let parent = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                let dialog = gtk::FileDialog::builder()
                    .title(bs::SNAPSHOT_RESTORE_BUTTON)
                    .build();
                let dest = dest.clone();
                let choose = btn.clone();
                choose.set_sensitive(false);
                dialog.select_folder(parent.as_ref(), gio::Cancellable::NONE, move |result| {
                    choose.set_sensitive(true);
                    if let Ok(file) = result
                        && let Some(path) = file.path()
                    {
                        dest.set_text(&path.display().to_string());
                    }
                });
            });
        }
        {
            let panel = Rc::clone(&panel);
            let client = Rc::clone(client);
            button.connect_clicked(move |_| panel.start(&client));
        }
        panel
    }

    fn sync_button(&self) {
        self.button
            .set_sensitive(!self.running.get() && !self.dest.text().trim().is_empty());
    }

    fn start(self: &Rc<Self>, client: &Rc<FaunaClient>) {
        let Some(snapshot_id) = self.snapshot_id.get() else {
            return;
        };
        let typed = self.dest.text().trim().to_string();
        if self.running.get() || typed.is_empty() {
            return;
        }
        let home = std::env::var_os("HOME");
        let Some(destination) =
            fauna_sync_engine::engine_lifecycle::resolve_restore_destination(&typed, home)
        else {
            self.progress.set_visible(false);
            self.result
                .set_text(&bs::snapshot_restore_destination_not_absolute(&typed));
            self.result.set_visible(true);
            return;
        };
        self.running.set(true);
        self.result.set_visible(false);
        self.progress.set_text(&bs::snapshot_restore_progress(
            &snapshot_id.to_string(),
            &destination.display().to_string(),
        ));
        self.progress.set_visible(true);
        self.sync_button();

        let (tx, rx) = async_channel::bounded::<String>(1);
        client.restore_snapshot_to_dir(snapshot_id, destination, tx);
        let panel = Rc::clone(self);
        glib::spawn_future_local(async move {
            let Ok(line) = rx.recv().await else { return };
            // A different snapshot opened meanwhile has already reset the
            // panel; its state is not this run's to paint.
            if panel.snapshot_id.get() == Some(snapshot_id) {
                panel.running.set(false);
                panel.progress.set_visible(false);
                panel.result.set_text(&line);
                panel.result.set_visible(true);
                panel.sync_button();
            }
        });
    }

    /// Repaint for the machine's opened detail: hidden with no snapshot open,
    /// reset (lines cleared) when another snapshot opens.
    pub fn update(&self, detail: Option<&fauna_backups_machine::SnapshotDetail>) {
        let id = detail.map(|d| d.snapshot_id);
        // A kind entry's snapshot restores from the Restore section below; the
        // whole-snapshot restore into a folder does not apply to it.
        self.widget
            .set_visible(detail.is_some_and(|d| d.message_kind.is_none()));
        if self.snapshot_id.get() != id {
            self.snapshot_id.set(id);
            self.progress.set_visible(false);
            self.result.set_visible(false);
            self.running.set(false);
            self.sync_button();
        }
    }
}
