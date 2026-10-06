//! GTK skeleton shared by the flat admin-tier bool-toggle pages —
//! `admin-contacts` and `admin-files` — the linux twin of
//! `fauna_client_mail_settings::bool_toggle_policy`'s own macro-generated
//! machine pair (round 68 of the shared-Rust lift sweep). Each page is a
//! dumb renderer of one focused `*PolicySnapshot` + dispatcher of one
//! `*PolicyAction`; the two instantiations (`admin_contacts.rs`,
//! `admin_files.rs`) were byte-for-byte identical modulo CardDAV/WebDAV
//! naming before this lift.
//!
//! `admin_calendar.rs` shares the skeleton but adds a second (port) control
//! and stays hand-written, mirroring `bool_toggle_policy`'s own "a fork for
//! one extra field would cost more macro-arm complexity than the ~40 lines
//! it would save" call.

/// Generates `build_$page_fn` plus its private widget/ctx/wire/render
/// plumbing for one boolean admin-tier toggle page. Invoke once per
/// protocol; write the invoking module's own `//!` doc with the
/// page-specific detail (which siblings it mirrors, the MDA "enabled if
/// any" clause, the UniFFI export name) — this macro only carries the parts
/// that are identical across every instantiation.
macro_rules! admin_bool_toggle_page {
    (
        page_fn: $page_fn:ident,
        machine: $Machine:ty,
        action: $Action:ident,
        snapshot: $Snapshot:ty,
        build_machine: $build_machine:path,
        strings: $S:path,
        icon: $icon:literal,
        heading_id: $heading_id:literal,
        toggle_id: $toggle_id:literal,
        wire_kind: $wire_kind:literal,
        set_variant: $SetVariant:ident,
        field: $field:ident,
    ) => {
        use adw::prelude::*;

        /// Handles to the widgets the snapshot renders into. GTK objects are
        /// reference-counted, so cloning this is cheap and shares widgets.
        #[derive(Clone)]
        struct Widgets {
            error_label: gtk::Label,
            enabled_toggle: gtk::Switch,
        }

        /// Per-page context threaded through hydrate / dispatch / render.
        struct Ctx {
            machine: std::sync::Arc<$Machine>,
            rt: tokio::runtime::Handle,
            /// Set while `render()` programmatically updates the toggle, so
            /// its `active-notify` handler doesn't echo the change back as
            /// an action.
            syncing: std::cell::Cell<bool>,
            w: Widgets,
        }

        /// Build the flat page.
        pub fn $page_fn() -> adw::PreferencesPage {
            use $S as S;
            let page = adw::PreferencesPage::builder()
                .title(S::TITLE)
                .icon_name($icon)
                .build();

            // --- Top group: heading + page-level error + the enable toggle.
            let top_group = adw::PreferencesGroup::builder()
                .title(S::TITLE)
                .description(S::DESCRIPTION)
                .build();
            top_group.set_header_suffix(Some(&crate::settings::marker($heading_id)));

            // error-message — page-level error label (Rule 2), hidden until set.
            let error_label = gtk::Label::builder().visible(false).build();
            crate::testid::set_test_id(&error_label, fauna_ui_ids::ERROR_MESSAGE);
            let error_row = adw::ActionRow::builder().activatable(false).build();
            error_row.add_suffix(&error_label);
            top_group.add(&error_row);

            // enabled-toggle — dispatches immediately on change (re-reads
            // persisted state).
            let enabled_toggle = crate::settings::switch_row(
                &top_group,
                S::ENABLED_LABEL,
                S::ENABLED_SUBTITLE,
                $toggle_id,
            );
            // tui's `admin::Action::Toggle*Enabled` (`account-data-plane.md`
            // § The offline-mutation contract — admin/provisioning is
            // OnlineOnly).
            crate::offline_gate::declare_wire_kind(&enabled_toggle, $wire_kind);

            page.add(&top_group);

            wire_machine(Widgets {
                error_label,
                enabled_toggle,
            });

            page
        }

        /// Connect the page to the shared policy machine, hydrate on mount,
        /// and wire the toggle. No-op (page stays at static placeholders)
        /// when no client is available — e.g. the unit test, which has no
        /// registered client.
        fn wire_machine(widgets: Widgets) {
            let client = match crate::settings::get_client() {
                Some(c) => c,
                None => return,
            };
            let machine = std::sync::Arc::new($build_machine(&client));

            let ctx = std::rc::Rc::new(Ctx {
                machine,
                rt: client.runtime_handle(),
                syncing: std::cell::Cell::new(false),
                w: widgets,
            });

            hydrate_and_render(&ctx);

            // enable toggle → Set*Enabled (skip the echo from render()).
            {
                let ctx = std::rc::Rc::clone(&ctx);
                ctx.w
                    .enabled_toggle
                    .clone()
                    .connect_active_notify(move |sw| {
                        if ctx.syncing.get() {
                            return;
                        }
                        dispatch_action(
                            &ctx,
                            $Action::$SetVariant {
                                enabled: sw.is_active(),
                            },
                        );
                    });
            }
        }

        /// Run `machine.hydrate()` on the tokio runtime (retrying while the
        /// WS socket comes up after login), then render the snapshot on the
        /// GTK main thread.
        fn hydrate_and_render(ctx: &std::rc::Rc<Ctx>) {
            let machine = std::sync::Arc::clone(&ctx.machine);
            let ctx_render = std::rc::Rc::clone(ctx);
            crate::async_helper::spawn_with_snapshot(
                &ctx.rt,
                move || async move {
                    let _ = machine.hydrate().await;
                    machine.snapshot()
                },
                move |snap| render(&ctx_render, &snap),
            );
        }

        /// Dispatch a fire-and-render action on the tokio runtime, then
        /// render the resulting snapshot.
        fn dispatch_action(ctx: &std::rc::Rc<Ctx>, action: $Action) {
            let machine = std::sync::Arc::clone(&ctx.machine);
            let ctx_render = std::rc::Rc::clone(ctx);
            crate::async_helper::spawn_with_snapshot(
                &ctx.rt,
                move || async move {
                    let _ = machine.dispatch(action).await;
                    machine.snapshot()
                },
                move |snap| render(&ctx_render, &snap),
            );
        }

        /// Render a snapshot into the page widgets (GTK main thread).
        fn render(ctx: &std::rc::Rc<Ctx>, snap: &$Snapshot) {
            let w = &ctx.w;

            crate::settings::render_error_label(&w.error_label, snap.error.as_deref());

            // enable toggle — reflect persisted state without echoing a dispatch.
            if w.enabled_toggle.is_active() != snap.$field {
                ctx.syncing.set(true);
                w.enabled_toggle.set_active(snap.$field);
                ctx.syncing.set(false);
            }
        }
    };
}

pub(super) use admin_bool_toggle_page;
