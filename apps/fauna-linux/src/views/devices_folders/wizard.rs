//! Folder creation wizard — a thin GTK renderer over the **embedded**
//! `FolderWizardMachine` (`libs/fauna-folders-machine`) owned by the page-level
//! `DevicesMachine` (`DevicesSnapshot.wizard`).
//!
//! The wizard is no longer constructed standalone: the Folders sub-page opens it via
//! `DevicesMachine::open_wizard()` and renders the dialog over the embedded
//! machine obtained from `DevicesMachine::wizard()`. The wizard machine's
//! observer forwards to the *page* observer (the `WizardObserverBridge` inside
//! `DevicesMachine::open_wizard`), so the page's single render loop drives this
//! dialog too: on each tick it calls [`FolderWizardView::render`], which swaps
//! the visible `gtk::Stack` page to `wiz.step()` and refreshes it off the
//! per-step snapshots. No wizard logic lives here — name/mode/device-roles/
//! review + `submit()` (WS-RPC `fauna.folders.create` + member adds)
//! all live in the machine. When `step() == Done` the page loop closes the dialog
//! (`DevicesMachine::close_wizard` + `refresh`).
//!
//! ui.yaml `devices` IDs: `wizard-name-input`, `wizard-device-check` (indexed),
//! `wizard-device-originates` / `-accepts` / `-applies-deletes` (indexed),
//! `wizard-next-button`, `wizard-back-button`, `wizard-create-button`,
//! `error-message`. The scan-frequency step (`wizard-frequency-option`) retired
//! with phase 5 (2026-08-20): the cadence is a constant, not a choice
//! (`file-sync.md` § Config, the phase-5 block).
//!
//! **Retired here by folders re-model phase 2 slice e** (2026-08-16, following
//! tui): the type step's `wizard-mode-sync` / `-backup` / `-web` /
//! `-backup-warning` (a folder has no type), `wizard-device-role` (replaced by
//! the three place-flag checkboxes above), and `wizard-retention-snapshots` /
//! `-days` (retention is the nest place's per-folder policy now, edited on any
//! folder's row). Do not reintroduce any of the seven.

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_devices_machine::DevicesMachine;
use fauna_folders_machine::{FolderWizardMachine, FolderWizardStep, WizardDevice};

use crate::async_helper;
use crate::i18n::strings;
use crate::testid::set_test_id;

// Stack child names — one per non-terminal `FolderWizardStep`.
const PAGE_NAME: &str = "name";
const PAGE_DEVICES: &str = "devices";
const PAGE_REVIEW: &str = "review";

fn step_to_page(step: FolderWizardStep) -> Option<&'static str> {
    Some(match step {
        FolderWizardStep::Name => PAGE_NAME,
        FolderWizardStep::Devices => PAGE_DEVICES,
        FolderWizardStep::Review => PAGE_REVIEW,
        // `Done` is terminal — the orchestrator closes the dialog instead.
        FolderWizardStep::Done => return None,
    })
}

type RefreshMap = Rc<RefCell<HashMap<&'static str, Rc<dyn Fn()>>>>;

/// The folder creation wizard dialog, rendered over the embedded
/// `FolderWizardMachine` owned by the page-level `DevicesMachine`. The page's
/// render loop owns the lifecycle: it constructs the view when
/// `DevicesSnapshot.wizard` first becomes `Some`, calls [`render`] on every tick,
/// and [`dismiss`]es it when the wizard reaches `Done` or the snapshot's wizard
/// goes back to `None`.
///
/// [`render`]: FolderWizardView::render
/// [`dismiss`]: FolderWizardView::dismiss
pub struct FolderWizardView {
    dialog: adw::Dialog,
    stack: gtk::Stack,
    refreshers: RefreshMap,
    back_btn: gtk::Button,
    next_btn: gtk::Button,
    create_btn: gtk::Button,
    /// Whether the declassify dialog is presented for the machine's current
    /// arm — the dialog is presented once per arm, never per render tick. The
    /// ARM itself is the machine's (`ReviewSnapshot.declassify_armed`); this
    /// only remembers that the modal answering it is already up.
    declassify_presented: Rc<Cell<bool>>,
}

impl FolderWizardView {
    /// Build + present the wizard dialog over `machine`'s embedded
    /// `FolderWizardMachine` (already created by `DevicesMachine::open_wizard`).
    /// All gestures forward to that embedded machine; its observer ticks the
    /// page, which re-renders this dialog via [`render`](Self::render). The
    /// dialog's `connect_closed` calls `DevicesMachine::close_wizard` so a
    /// user-dismissed dialog clears the page state.
    pub fn open(parent: &impl IsA<gtk::Widget>, machine: &Arc<DevicesMachine>) -> Self {
        let wiz = machine
            .wizard()
            .expect("FolderWizardView::open called with no wizard open");

        // ── Step stack + per-page registry ──────────────────────────────
        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::SlideLeftRight);
        stack.set_transition_duration(150);
        stack.set_vexpand(true);

        let refreshers: RefreshMap = Rc::new(RefCell::new(HashMap::new()));
        let (p1, r1) = build_name(&wiz);
        let (p2, r2) = build_devices_step(&wiz);
        let (p3, r3) = build_review(&wiz);
        stack.add_named(&p1, Some(PAGE_NAME));
        stack.add_named(&p2, Some(PAGE_DEVICES));
        stack.add_named(&p3, Some(PAGE_REVIEW));
        {
            let mut map = refreshers.borrow_mut();
            map.insert(PAGE_NAME, r1);
            map.insert(PAGE_DEVICES, r2);
            map.insert(PAGE_REVIEW, r3);
        }

        // ── Footer nav (shared across steps) ────────────────────────────
        let back_btn = gtk::Button::with_label(strings::devices::wizard::BACK);
        set_test_id(&back_btn, ids::WIZARD_BACK_BUTTON);
        {
            let wiz = wiz.clone();
            back_btn.connect_clicked(move |_| wiz.back());
        }

        let next_btn = gtk::Button::with_label(strings::devices::wizard::NEXT);
        next_btn.add_css_class("suggested-action");
        set_test_id(&next_btn, ids::WIZARD_NEXT_BUTTON);
        {
            let wiz = wiz.clone();
            next_btn.connect_clicked(move |_| wiz.next());
        }

        let create_btn = gtk::Button::with_label(strings::devices::wizard::CREATE);
        create_btn.add_css_class("suggested-action");
        set_test_id(&create_btn, ids::WIZARD_CREATE_BUTTON);
        // The ceremony is decided by the write that BINDS it: `submit()` calls
        // `fauna.folders.create` first and only then adds each enrolled device
        // (`FolderWizardMachine::submit` phases 2–3) — a create that failed has
        // left nothing to add to. Back/Next and every step input are wizard
        // state, so they declare nothing.
        crate::offline_gate::declare_wire_kind(&create_btn, "fauna.folders.create");
        {
            let wiz = wiz.clone();
            create_btn.connect_clicked(move |_| {
                let wiz = wiz.clone();
                // `submit()` ticks `on_changed()` (Submitting → Failed/Done) via
                // the page observer, so the page render loop drives the UI; the
                // result callback is a no-op.
                async_helper::run_on_tokio(async move { wiz.submit().await }, |_step| {});
            });
        }

        let footer = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::End)
            .margin_top(12)
            .build();
        footer.append(&back_btn);
        footer.append(&next_btn);
        footer.append(&create_btn);

        // ── Layout ──────────────────────────────────────────────────────
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();
        content.append(&stack);
        content.append(&footer);

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let title = gtk::Label::new(Some(strings::devices::wizard::NEW_FOLDER));
        title.add_css_class("title-4");
        header.set_title_widget(Some(&title));
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&content));

        let dialog = adw::Dialog::builder()
            .title(strings::devices::wizard::NEW_FOLDER)
            .content_width(560)
            .content_height(560)
            .child(&toolbar)
            .build();

        // A user-dismissed dialog (X / Esc) clears the page's wizard state. On a
        // page-driven close (Done) this fires too, but `close_wizard` is
        // idempotent.
        {
            let machine = Arc::clone(machine);
            dialog.connect_closed(move |_| machine.close_wizard());
        }

        let view = Self {
            dialog,
            stack,
            refreshers,
            back_btn,
            next_btn,
            create_btn,
            declassify_presented: Rc::new(Cell::new(false)),
        };
        view.render(&wiz);
        view.dialog.present(Some(parent));
        view
    }

    /// Swap the visible step page to `wiz.step()`, refresh it off the per-step
    /// snapshots, and gate the footer buttons. Called by the page render loop on
    /// every observer tick (the `Done` step is handled by the loop, which closes
    /// the dialog, so this no-ops on it).
    pub fn render(&self, wiz: &Arc<FolderWizardMachine>) {
        let step = wiz.step();
        let Some(page) = step_to_page(step) else {
            return; // Done — the page loop dismisses the dialog.
        };

        if self.stack.visible_child_name().as_deref() != Some(page) {
            self.stack.set_visible_child_name(page);
        }
        if let Some(refresh) = self.refreshers.borrow().get(page) {
            refresh();
        }

        // Footer: Back from step 2+, Next on steps 1–2, Create only on Review.
        self.back_btn.set_visible(step != FolderWizardStep::Name);
        let on_nav = matches!(step, FolderWizardStep::Name | FolderWizardStep::Devices);
        self.next_btn.set_visible(on_nav);
        if on_nav {
            // Only the name step can gate Next (needs a non-empty name).
            let enabled = step != FolderWizardStep::Name || wiz.name_snapshot().continue_enabled;
            self.next_btn.set_sensitive(enabled);
        }
        let on_review = step == FolderWizardStep::Review;
        self.create_btn.set_visible(on_review);
        if on_review {
            let review = wiz.review_snapshot();
            // While the declassify confirm asks, Create stays painted but dim:
            // answering the dialog is the gesture that creates.
            self.create_btn
                .set_sensitive(review.create_enabled && !review.declassify_armed);
            self.present_declassify_if_armed(wiz, review.declassify_armed);
        } else {
            self.declassify_presented.set(false);
        }
    }

    /// A Public wizard's Create armed the owner row's SAME declassify dialog
    /// (`ui/folders.md` § Audience and website serving → *Born public*,
    /// ruling 2; the builder is shared with the row,
    /// [`super::folders::present_declassify_confirm`]). The confirm answers
    /// `confirm_public_create()`, which creates the folder public and attests
    /// it; cancel / Escape / close answers `cancel_public_create()`, which
    /// sends nothing and stays on Review.
    fn present_declassify_if_armed(&self, wiz: &Arc<FolderWizardMachine>, armed: bool) {
        if !armed {
            self.declassify_presented.set(false);
            return;
        }
        if self.declassify_presented.replace(true) {
            return;
        }
        let confirm_wiz = wiz.clone();
        let decline_wiz = wiz.clone();
        super::folders::present_declassify_confirm(
            &self.create_btn,
            move || {
                let wiz = confirm_wiz.clone();
                // Ticks `on_changed()` (Submitting → Failed/Done) through the
                // page observer, exactly as `submit()` does.
                async_helper::run_on_tokio(
                    async move { wiz.confirm_public_create().await },
                    |_step| {},
                );
            },
            Some(Box::new(move || decline_wiz.cancel_public_create())),
            "fauna.folders.create",
        );
    }

    /// Close the dialog. Idempotent — safe to call after a user dismiss.
    pub fn dismiss(&self) {
        self.dialog.close();
    }
}

// ── Step 1: name ────────────────────────────────────────────────────────
//
// A folder has NO TYPE (folders re-model phase 2 slice e): the three
// `wizard-mode-*` buttons and `wizard-mode-backup-warning` are retired from
// ui.yaml, so this step is the name and nothing else. What a device does is its
// place flags (step 2); what the nest keeps is the nest place's snapshot policy,
// edited on any folder's row (`folder-nest-*`). New folders are created `sync`,
// which the machine already defaults to.
//
// ⚠ Retiring `wizard-mode-web` leaves website-folder creation unreachable until
// phase 4 mints the website toggle. That was surfaced to the user and accepted
// 2026-08-15 (the feature has no users); do NOT re-add a mode control to close
// it — phase 4 owns the fix.

fn build_name(m: &Arc<FolderWizardMachine>) -> (gtk::Box, Rc<dyn Fn()>) {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();

    let name_entry = gtk::Entry::builder()
        .placeholder_text(strings::devices::wizard::NAME_PLACEHOLDER)
        .build();
    set_test_id(&name_entry, ids::WIZARD_NAME_INPUT);
    {
        let m = m.clone();
        name_entry.connect_changed(move |e| m.set_name(e.text().to_string()));
    }
    root.append(&name_entry);

    // The disabled-Next explainer: `continue_enabled` gates on a non-empty name,
    // and a greyed-out Next with no on-screen reason was a live-user
    // "unknowable" report (tui carries the same line).
    let name_required = gtk::Label::builder()
        .label(strings::devices::wizard::NAME_REQUIRED)
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .css_classes(["fauna-muted"])
        .build();
    root.append(&name_required);

    // Who can see the new folder — a field of this step, never a step of its
    // own (`ui/folders.md` § Audience and website serving → *Born public*,
    // ruling 1). Absent on a seam that cannot attest a born-public create (a
    // control that cannot work must not paint); `audience_offered` is read once
    // at the machine's construction, so it is read once here too. A new folder
    // is unbound, so the shared option set is exactly Private | Public. The
    // value/label DropDown split of the owner row's `folder-audience-select`:
    // the MODEL strings are the wire values the cross-app `select(id, value)` /
    // `get_text` contract drives. The select paints the user's choice —
    // nothing exists yet, so there is no current audience to snap back to.
    let audience_widgets = m.name_snapshot().audience_offered.then(|| {
        let opts = fauna_folders_machine::audience_options(false, &m.name_snapshot().audience);
        let values: Vec<String> = opts.iter().map(|o| o.value.clone()).collect();
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        let dropdown = gtk::DropDown::builder()
            .model(&gtk::StringList::new(&refs))
            .build();
        let label_expr = gtk::ClosureExpression::new::<String>(
            &[] as &[gtk::Expression],
            gtk::glib::closure!(|item: gtk::StringObject| {
                fauna_folders_machine::audience_label(item.string().as_str())
                    .resolve(strings::lookup)
            }),
        );
        dropdown.set_expression(Some(&label_expr));
        set_test_id(&dropdown, ids::WIZARD_AUDIENCE_SELECT);
        let current = m.name_snapshot().audience;
        if let Some(idx) = values.iter().position(|v| *v == current) {
            dropdown.set_selected(idx as u32);
        }
        {
            let m = m.clone();
            let values = values.clone();
            dropdown.connect_selected_notify(move |dd| {
                if let Some(value) = values.get(dd.selected() as usize)
                    && *value != m.name_snapshot().audience
                {
                    m.set_audience(value.clone());
                }
            });
        }
        let field = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        let caption = gtk::Label::builder()
            .label(strings::devices::FOLDER_AUDIENCE)
            .xalign(0.0)
            .build();
        field.append(&caption);
        field.append(&dropdown);
        root.append(&field);
        let hint = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .css_classes(["fauna-muted"])
            .build();
        set_test_id(&hint, ids::WIZARD_AUDIENCE_HINT);
        root.append(&hint);
        (dropdown, values, hint)
    });

    let refresh: Rc<dyn Fn()> = Rc::new({
        let m = m.clone();
        let name_entry = name_entry.clone();
        let name_required = name_required.clone();
        move || {
            let snap = m.name_snapshot();
            if name_entry.text() != snap.name.as_str() {
                name_entry.set_text(&snap.name);
            }
            name_required.set_visible(!snap.continue_enabled);
            if let Some((dropdown, values, hint)) = &audience_widgets {
                if let Some(idx) = values.iter().position(|v| *v == snap.audience)
                    && dropdown.selected() != idx as u32
                {
                    dropdown.set_selected(idx as u32);
                }
                hint.set_label(
                    &fauna_folders_machine::audience_hint(false, &snap.audience)
                        .resolve(strings::lookup),
                );
            }
        }
    });

    (root, refresh)
}

// ── Step 2: per-device enrollment + place flags ─────────────────────────
//
// The `wizard-device-role` Source/Sync/Backup/Mirror picker is RETIRED (phase 2
// slice e): a live user called those labels "a completely incomprehensible list
// of things" (2026-08-05), and the design's answer is not better role nouns but
// three checkboxes that each say what they do.

/// The three place-flag checkboxes, in canonical order: element id, label, its
/// one-line explainer, and which flag of [`WizardDevice`] it reads.
const PLACE_FLAG_BOXES: [(&str, &str, &str, PlaceFlagKind); 3] = [
    (
        "wizard-device-originates",
        strings::devices::wizard::PLACE_ORIGINATES,
        strings::devices::wizard::PLACE_ORIGINATES_DESC,
        PlaceFlagKind::Originates,
    ),
    (
        "wizard-device-accepts",
        strings::devices::wizard::PLACE_ACCEPTS,
        strings::devices::wizard::PLACE_ACCEPTS_DESC,
        PlaceFlagKind::Accepts,
    ),
    (
        "wizard-device-applies-deletes",
        strings::devices::wizard::PLACE_APPLIES_DELETES,
        strings::devices::wizard::PLACE_APPLIES_DELETES_DESC,
        PlaceFlagKind::AppliesDeletes,
    ),
];

/// Which of a seat's three flags a checkbox owns. The flag *meanings* live once,
/// in `fauna_protocol::folders::PlaceFlags`; this only picks the field.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PlaceFlagKind {
    Originates,
    Accepts,
    AppliesDeletes,
}

impl PlaceFlagKind {
    fn read(self, d: &WizardDevice) -> bool {
        match self {
            Self::Originates => d.originates,
            Self::Accepts => d.accepts,
            Self::AppliesDeletes => d.applies_deletes,
        }
    }

    /// The seat's flag triple with this one replaced — the shape
    /// `set_device_flags` takes, which is whole-value like the nest write.
    fn with(self, d: &WizardDevice, on: bool) -> (bool, bool, bool) {
        match self {
            Self::Originates => (on, d.accepts, d.applies_deletes),
            Self::Accepts => (d.originates, on, d.applies_deletes),
            Self::AppliesDeletes => (d.originates, d.accepts, on),
        }
    }
}

fn build_devices_step(m: &Arc<FolderWizardMachine>) -> (gtk::Box, Rc<dyn Fn()>) {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();

    let heading = gtk::Label::builder()
        .label(strings::devices::wizard::SELECT_DEVICES_ROLES)
        .xalign(0.0)
        .build();
    root.append(&heading);

    let devices = m.device_places_snapshot().devices;

    if devices.is_empty() {
        let empty = gtk::Label::builder()
            .label(strings::devices::wizard::NO_DEVICES_AVAILABLE)
            .xalign(0.0)
            .css_classes(["fauna-muted"])
            .build();
        root.append(&empty);
        // Static page — no per-device state to refresh.
        return (root, Rc::new(|| {}));
    }

    // Build one block per device once (the device set is fixed at construction);
    // refresh only mirrors `selected` / the flags / the refusal back onto the
    // widgets.
    let mut checks: Vec<gtk::CheckButton> = Vec::with_capacity(devices.len());
    let mut flag_boxes: Vec<Vec<gtk::CheckButton>> = Vec::with_capacity(devices.len());
    for (i, dev) in devices.iter().enumerate() {
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();

        // The device's name IS the check's label, not a sibling `gtk::Label`:
        // clicking the name toggles the box, and the check's own text says which
        // device it enrolls — the cross-app `get_text` contract for a checkbox
        // (tui's element carries the label; web's `<label>` wraps the input).
        let check = gtk::CheckButton::with_label(&dev.label);
        check.set_hexpand(true);
        set_test_id(&check, ids::WIZARD_DEVICE_CHECK);
        {
            let m = m.clone();
            check.connect_toggled(move |cb| {
                let want = cb.is_active();
                let cur = m
                    .device_places_snapshot()
                    .devices
                    .get(i)
                    .map(|d| d.selected)
                    .unwrap_or(false);
                if want != cur {
                    m.toggle_device_member(i as u32);
                }
            });
        }
        row.append(&check);
        root.append(&row);
        checks.push(check);

        let mut boxes = Vec::with_capacity(PLACE_FLAG_BOXES.len());
        for (id, label, desc, kind) in PLACE_FLAG_BOXES {
            let flag_box = gtk::CheckButton::with_label(label);
            flag_box.set_margin_start(24);
            set_test_id(&flag_box, id);
            {
                let m = m.clone();
                flag_box.connect_toggled(move |cb| {
                    let on = cb.is_active();
                    let Some(d) = m.device_places_snapshot().devices.into_iter().nth(i) else {
                        return;
                    };
                    if kind.read(&d) == on {
                        return; // refresh echo, not a user gesture
                    }
                    let (originates, accepts, applies_deletes) = kind.with(&d, on);
                    m.set_device_flags(i as u32, originates, accepts, applies_deletes);
                });
            }
            root.append(&flag_box);
            boxes.push(flag_box);

            // Each box carries its own one-line explainer — the pattern the
            // retired role picker used per seat, now per box, because the flags
            // are the thing being explained.
            let desc_label = gtk::Label::builder()
                .label(desc)
                .wrap(true)
                .xalign(0.0)
                .margin_start(48)
                .css_classes(["fauna-muted"])
                .build();
            root.append(&desc_label);
        }
        flag_boxes.push(boxes);
    }

    let refresh: Rc<dyn Fn()> = Rc::new({
        let m = m.clone();
        move || {
            let snap = m.device_places_snapshot();
            for (i, dev) in snap.devices.iter().enumerate() {
                if let Some(c) = checks.get(i)
                    && c.is_active() != dev.selected
                {
                    c.set_active(dev.selected);
                }
                if let Some(boxes) = flag_boxes.get(i) {
                    for (b, (_, _, _, kind)) in boxes.iter().zip(PLACE_FLAG_BOXES) {
                        let want = kind.read(dev);
                        if b.is_active() != want {
                            b.set_active(want);
                        }
                        // The cross-app `on`/`off` state the place editor's
                        // boxes publish too (an unmarked CheckButton reads
                        // `true`/`false`), mirrored from the machine.
                        crate::testid::set_test_attr(b, "state", if want { "on" } else { "off" });
                    }
                }
            }
        }
    });

    (root, refresh)
}

// ── Step 3: read-only review ────────────────────────────────────────────

fn build_review(m: &Arc<FolderWizardMachine>) -> (gtk::Box, Rc<dyn Fn()>) {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();

    let summary = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    root.append(&summary);

    let error_label = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .css_classes(["error"])
        .visible(false)
        .build();
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    root.append(&error_label);

    let refresh: Rc<dyn Fn()> = Rc::new({
        let m = m.clone();
        let summary = summary.clone();
        let error_label = error_label.clone();
        move || {
            // Read-only — rebuild the summary rows each tick.
            while let Some(child) = summary.first_child() {
                summary.remove(&child);
            }
            let snap = m.review_snapshot();
            append_review_row(&summary, strings::devices::wizard::REVIEW_NAME, &snap.name);
            // Who can see it — the Name step's choice, read-only like the name
            // (Born public, ruling 1).
            append_review_row(
                &summary,
                strings::devices::wizard::REVIEW_AUDIENCE,
                &fauna_folders_machine::audience_label(&snap.audience).resolve(strings::lookup),
            );
            // No mode line, no retention line, no cadence line: a folder has
            // no type (phase 2 slice e), retention is the nest place's policy
            // edited on the row rather than chosen at create, and the scan
            // cadence is a constant, not a choice (phase 5).
            // The enrolled list names devices, not roles — the role picker is
            // retired, and a seat's place is now three flags, which a review
            // line cannot summarize honestly in one noun.
            let devices_text = if snap.enrolled.is_empty() {
                strings::devices::wizard::REVIEW_NO_DEVICES.to_string()
            } else {
                snap.enrolled
                    .iter()
                    .map(|d| d.label.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            append_review_row(
                &summary,
                strings::devices::wizard::REVIEW_DEVICES,
                &devices_text,
            );

            let text = snap.error.map(|err| err.resolve(strings::lookup));
            crate::settings::render_error_label(&error_label, text.as_deref());
        }
    });

    (root, refresh)
}

// ── Small helpers ───────────────────────────────────────────────────────

fn append_review_row(parent: &gtk::Box, key: &str, value: &str) {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let k = gtk::Label::builder()
        .label(key)
        .xalign(0.0)
        .css_classes(["fauna-muted"])
        .build();
    let v = gtk::Label::builder()
        .label(value)
        .xalign(0.0)
        .hexpand(true)
        .wrap(true)
        .build();
    row.append(&k);
    row.append(&v);
    parent.append(&row);
}
