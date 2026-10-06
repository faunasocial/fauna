pub mod destinations;
pub mod file_list;
pub mod restore;
pub mod snapshot_list;

use adw::prelude::*;
use std::rc::Rc;
use std::sync::Arc;

use fauna_backups_machine::{BackupsMachine, BackupsObserver, build_backups_machine};

use crate::client::FaunaClient;

/// Handles for backups view widgets that need dynamic updates.
pub struct BackupsHandles {
    // The snapshot half's widgets are deliberately NOT surfaced here. They are
    // owned by the page's own render loop, which is the only thing that writes
    // them — every mutation is a `BackupsMachine` gesture, so there is nothing
    // for app.rs to poke. Handing them out again is how a second writer, with
    // its own idea of the page's state, used to get in.
    /// The file list box — populated from the machine's opened detail.
    pub snapshot_file_list_box: gtk::ListBox,
    pub detail_header: adw::HeaderBar,
    /// The shared-Rust `BackupsMachine`. app.rs refreshes it on auth; the page
    /// refreshes it again on every nav (`connect_map`).
    pub machine: Arc<BackupsMachine>,
    /// Restore surfaces (history list, divergence banner/modal, local
    /// restore action) — see `restore.rs`.
    pub restore: restore::RestoreHandles,
}

/// Bridges `BackupsMachine` notifications to the GTK main loop: each
/// `on_changed` pushes a tick onto an `async-channel` the render loop drains.
/// The same `GtkObserver` shape the Devices/Folders pages use — gestures run
/// on the client's tokio runtime, so the thread-safe sender is the hand-off.
struct GtkBackupsObserver {
    tx: async_channel::Sender<()>,
}

impl BackupsObserver for GtkBackupsObserver {
    fn on_changed(&self) {
        let _ = self.tx.try_send(());
    }
}

/// Build the full backups view: navigation split with snapshot list on the
/// left and file list on the right.
/// Returns the split view and widget handles.
pub fn build_backups_view(client: &Rc<FaunaClient>) -> (adw::NavigationSplitView, BackupsHandles) {
    // ── Machine + observer wiring ───────────────────────────────────────
    let (tx, rx) = crate::async_helper::snapshot_wake_channel();
    let observer: Arc<dyn BackupsObserver> = Arc::new(GtkBackupsObserver { tx });
    // Custody is a construction input, so a sealed set's snapshot paths arrive
    // opened rather than as envelopes (`behavior/path-sealing.md` § THE
    // CONSUMER-WIRING RULE) — the machine never bypasses sealing, it is wired
    // through it.
    let machine = build_backups_machine(
        Arc::clone(client.nest_rpc()),
        observer,
        client.label_custody(),
    );
    // Row provenance: manual creates stamp this shell's stable sync device id
    // when it has one. A device with no id records an unattributed snapshot
    // rather than refusing the capture.
    machine.set_device_id(crate::sync::device_id().ok().map(|id| id.to_vec()));

    let (list_widget, snapshot_handles) = snapshot_list::build_snapshot_list(client, &machine);

    let list_page = adw::NavigationPage::builder()
        .title(crate::i18n::strings::backups::TITLE)
        .child(&list_widget)
        .build();

    // Content holds the snapshot file list (when one is selected) plus the
    // always-visible restore surfaces (history + local restore action).
    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // Pre-build the file list widget for snapshot detail.
    let (file_list_widget, snapshot_file_list_box, detail_header) =
        file_list::build_snapshot_file_list(crate::i18n::strings::backups::SNAPSHOT_TITLE);
    content_box.append(&file_list_widget);

    // Restore surfaces + backup-destination management — scrollable together so
    // the history list, restore action card, and destination list all fit.
    let (restore_box, restore_handles) = restore::build_restore_section(client);
    let (audit_alerts, destinations_box, error_label) =
        destinations::build_destinations_section(client);
    let scroll_inner = gtk::Box::new(gtk::Orientation::Vertical, 0);
    scroll_inner.append(&restore_box);
    scroll_inner.append(&destinations_box);

    // `backup-audit-alert` banners sit above the scroller, at the top of the
    // page and outside it: a warning that a backup is not keeping up must be
    // visible without scrolling past the snapshot surfaces to find it
    // (`docs/goal/ui/backups.md` § Audit-alert surface). Hidden while every
    // destination is healthy.
    content_box.append(&audit_alerts);
    let restore_scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&scroll_inner)
        .build();
    content_box.append(&restore_scrolled);

    let detail_page = adw::NavigationPage::builder()
        .title(crate::i18n::strings::backups::SNAPSHOT_TITLE)
        .child(&content_box)
        .build();

    let split = adw::NavigationSplitView::new();
    split.set_sidebar(Some(&list_page));
    split.set_content(Some(&detail_page));

    // ── Render loop ─────────────────────────────────────────────────────
    // One pass per observer tick paints the whole snapshot half off a single
    // `machine.snapshot()`, so no two surfaces can read different states.
    {
        let machine = Arc::clone(&machine);
        let client = Rc::clone(client);
        let file_list_box = snapshot_file_list_box.clone();
        let error_label = error_label.clone();
        // `SnapshotPaneHandles` lives in the returned `BackupsHandles`, so the
        // loop keeps its own clone — a refcount bump over the same widgets.
        let panes = snapshot_handles.clone();
        crate::async_helper::spawn_wake_loop(rx, move || {
            {
                let page = machine.snapshot();
                snapshot_list::render_snapshot_pane(&panes, &machine, &page, &client);
                file_list::populate_snapshot_file_list(
                    &file_list_box,
                    page.detail.as_ref(),
                    &client,
                );
                // The machine's `error` is the page's ONE `error-message`
                // (e2e Rule 2). A completed check with errors is NOT one — it
                // reports through the check-result surface (rule 6).
                let text = page
                    .error
                    .as_ref()
                    .map(|t| t.clone().resolve(crate::i18n::strings::lookup));
                crate::settings::render_error_label(&error_label, text.as_deref());
            }
            glib::ControlFlow::Continue
        });
    }

    // Refresh the machine off WS-RPC whenever the page becomes visible, the
    // same `connect_map` shape the Devices / Folders sub-pages use.
    {
        let machine = Arc::clone(&machine);
        let handle = client.runtime_handle();
        split.connect_map(move |_| {
            let machine = Arc::clone(&machine);
            handle.spawn(async move { machine.refresh().await });
        });
    }

    let handles = BackupsHandles {
        snapshot_file_list_box,
        detail_header,
        machine,
        restore: restore_handles,
    };

    (split, handles)
}
