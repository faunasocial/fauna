#[cfg(test)]
mod account_registry_census_test;
mod account_runtime;
mod account_scope;
mod actor_scope;
mod app;
mod app_settings;
mod async_helper;
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
mod automation;
mod autostart;
mod backup_audit;
mod blocking_flush;
mod c2pa;
mod client;
mod clipboard;
mod confirm_dialog;
#[cfg(test)]
mod content_index_smoke_test;
mod content_policy;
mod conversations;
mod critical_alerts;
mod debounce;
mod drafts_autosave;
mod feed;
mod i18n;
mod instance_remote;
mod launcher_badge;
mod logs_view;
mod mail_glue;
mod main_loop_meter;
mod media_loads;
mod mls;
mod notifications;
mod offline_gate;
/// The co-present offline share-initiation ceremony (`docs/goal/behavior/p2p.md`
/// § Offline share initiation — tui leads the affordance; linux is the second
/// app).
#[cfg(feature = "p2p-share")]
mod offline_share;
mod p2p;
#[cfg(test)]
mod packaging_identity_test;
mod qr_widget;
mod region;
mod rows;
mod screen_lock;
mod search;
mod service_watcher;
mod settings;
/// The peer-transfer plane's app host (`docs/goal/behavior/p2p.md`
/// § Cross-user shared-set transfer — tui leads; linux is the second app). The plane's decisions live in
/// `fauna_sync_engine::share_glue`; this is the GTK-shaped half.
#[cfg(feature = "p2p-share")]
mod share_glue;
mod source_glyph;
mod store_surfaces;
mod subscriptions_author;
mod succession_aftermath;
mod supervision_snapshot;
mod sync;
mod sync_agent;
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
mod test_agent;
pub mod testid;
mod tray;
mod updater;
mod url_opener;
mod views;
#[cfg(test)]
mod walk;
mod ward_asks;
mod window_state;
mod wire_kind_dropdown;

use adw::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use std::rc::Rc;
use std::sync::atomic::Ordering;

/// The app's one identifier — GtkApplication id, Flatpak app-id, AppStream
/// `<id>`, and the installed desktop entry's basename, all the same string
/// (`installers/macos.md` § Identifier domain → *How the LEAF is spelled*,
/// ratified 2026-08-22).
///
/// The leaf is the **product name, lowercase** — the same leaf every registry
/// we get to choose on accepts, so the stutter is convergent uniformity rather
/// than a Linux quirk. It is not a platform word and not a generic one: Flathub
/// **bans** a `.desktop`, `.app` or `.linux` leaf outright as generic, and
/// AppStream additionally *strips* a trailing `.desktop` as the legacy
/// desktop-entry suffix — which is what the previous spelling,
/// `social.fauna.desktop`, ran into on both counts. Lowercase follows the only
/// layers with written guidance (Flatpak conventions and the AppStream spec);
/// both cases are legal everywhere, which is why the case is *tested* rather
/// than enforced by any tool. `packaging_identity_test.rs` pins every file that
/// re-types this string, and the case itself.
const APP_ID: &str = "social.fauna.fauna";

fn main() {
    // Handle --version before GTK init (GTK takes over argv).
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("fauna-desktop {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // Install the shared tracing subscriber FIRST — before any tracing event —
    // so the whole session is captured to the in-memory ring (Settings → Logs)
    // and the rolling on-disk file. See client::install_logging.
    client::install_logging();

    // Start the system tray icon (runs on its own D-Bus thread).
    let _tray = tray::start_tray();

    // In e2e mode (driven by the AT-SPI bridge) run with NON_UNIQUE so each
    // launch is its own GApplication primary. fauna-desktop has a fixed app id
    // (`social.fauna.fauna`); on the shared D-Bus session bus the first
    // instance owns the primary slot and any later launch becomes a *remote*
    // that activates it and exits its own GTK loop — never registering with
    // AT-SPI. That makes concurrent linux e2e runs (one session's full suite +
    // another's single test) collide and time out. NON_UNIQUE removes the slot
    // contention entirely. Gated to e2e so production keeps single-instance
    // behaviour (tray raise, notification re-activation, etc.).
    // See tests/e2e-unified/drivers/linux.py::_bridge_command_and_env.
    //
    // A *bound* launch (`FAUNA_BOUND_ACCOUNT` — account-scoping.md
    // § Concurrent instances) must be its own primary too: GApplication's
    // D-Bus uniqueness stays the friendly raise-on-relaunch layer for PLAIN
    // launches only (the desktop-icon / tray / notification channel — exactly
    // the role LaunchServices plays on macOS), while each bound launch runs
    // as its own process and the shared per-account `AccountInstanceLock` —
    // not the bus name — decides whether the account is free (acquired in
    // `app.rs`'s `AuthSuccess` arm; wizard-routed bound launches refuse in
    // `build_ui` below). Production keeps single-instance raise behaviour for
    // every launch that carries no binding.
    // Launch-collision detection, BEFORE GApplication is built — the one
    // moment it can happen (account-scoping.md § Concurrent instances → the
    // colliding instance's surface). A plain launch whose would-be account is
    // already served must render the chooser, and GApplication's D-Bus
    // uniqueness would otherwise redirect this process into the running
    // instance and exit it before any of our code runs. So we ask first, and
    // opt out of the redirect exactly when the answer is "collision".
    //
    // Opting out is what makes the chooser's exits work rather than a
    // contradiction: the *running* plain instance keeps the well-known name,
    // so `instance_remote` can still reach it to raise or to forward an
    // add-account intent. The probe is display-only — arbitration stays at
    // `become_session_instance`'s acquire.
    let collided = launch_collision_detected();
    let mut builder = adw::Application::builder().application_id(APP_ID);
    if e2e_mode_enabled() || fauna_client_accounts::requested_bound_account().is_some() || collided
    {
        builder = builder.flags(gio::ApplicationFlags::NON_UNIQUE);
    }
    let application = builder.build();

    // Hold the application open across window teardown→rebuild transitions
    // (sign-out, onboarding↔main, launch→main), where the old window is
    // destroyed and the next one is built in the same callback — without the
    // hold a transient zero-window moment could trip GtkApplication's
    // auto-quit-on-last-window. The guard is leaked intentionally; it lives for
    // the whole process. Close-to-tray (window hidden, not destroyed) also
    // relies on the registered window keeping the app alive. Deliberate quits
    // are therefore explicit: Ctrl+Q, the tray "Quit" item, and the
    // close-to-tray-off path in `connect_close_request`.
    application.connect_startup(|app| {
        load_css();
        settings::general::apply_saved_theme();
        // Close-to-tray is a persisted user choice, so it must be restored
        // before any window can be closed. It defaults ON (the app hosts the
        // in-process sync engine — closing a window must not stop sync); the
        // hide itself stays gated on a live tray host by
        // `tray::should_hide_to_tray`.
        app_settings::apply_saved_close_to_tray();

        // Start the services.json watcher for sidecar lifecycle management.
        if let Some(data_dir) = service_watcher::resolve_data_dir() {
            let _handle = service_watcher::start(data_dir);
        }

        // Install the disk-backed nest-identity pin store before the first
        // authenticated connect, so TOFU pins (self-signed / LAN nests) survive
        // restarts instead of being re-learned every launch (security.md
        // § Transport trust). Replaces the process-global in-memory default.
        client::install_disk_pin_store();

        std::mem::forget(app.hold());

        // Global raise reader for the app's two out-of-band raise sources.
        // Lives for the whole app lifetime so both work in every state:
        // authenticated, onboarding, or post-signout (no window).
        // `app.activate()` re-enters `build_ui`, which presents an existing
        // window or builds one from stored credentials.
        //
        // 1. `tray::TRAY_RAISE` — the tray's "Open Fauna" menu.
        // 2. `instance_remote::RAISE_REQUESTED` — the per-account activation
        //    endpoint's `Activate` (`account-scoping.md` § the per-(OS login,
        //    account) raise channel): a colliding launch asking the instance
        //    that serves its would-be account to come forward. Drained here
        //    rather than raising from the D-Bus handler because that handler
        //    runs the reply path; hopping to the main loop keeps the caller's
        //    call bounded.
        let app_for_raise = app.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            if tray::TRAY_RAISE.swap(false, Ordering::SeqCst)
                || instance_remote::RAISE_REQUESTED.swap(false, Ordering::SeqCst)
            {
                app_for_raise.activate();
            }
            glib::ControlFlow::Continue
        });

        // Tray unread badge + DM message OS toasts — both derived from the shared
        // `ConversationsManager` snapshot, the unified source that replaces the
        // deleted legacy fauna-native inbox HTTP drain (`fetch_inbox` →
        // `InboxLoaded`). Driven from app-startup scope so it lives for the whole
        // process and holds exactly one observer at a time — never one per
        // `build_ui` build→teardown cycle, which would leak an observer each
        // rebuild. Mirrors the conversations view's observer loop but writes the
        // tray mutex (ksni reads it live in `status()` / `tool_tip()`) and fires
        // toasts instead of rendering widgets.
        //
        // ⚠ **Re-attaching is load-bearing, not defensive** (fixed 2026-09-20).
        // `manager().clear_observers()` runs at four sites — sign-out, account
        // switch, a failed-trust re-launch, and the agent's `reset`/`logout` —
        // and it drops EVERY observer, this app-lifetime one included. Its own
        // comment ("the next login re-attaches a live observer") holds only for
        // the per-window observer `views/conversations` builds: nothing
        // re-attached this one, so from the first sign-out onward the process
        // fired no DM toast and froze its tray unread badge for good, with
        // nothing on screen to say so. The loop therefore owns its own liveness
        // — a closed channel means "the observer I registered was dropped", so
        // it registers a new one and carries on — rather than asking four
        // teardown sites to remember it.
        glib::MainContext::default().spawn_local(async move {
            loop {
                let conv_rx =
                    crate::conversations::observer::attach(&crate::conversations::manager());
                // OS notifications: the native toast *firing* is client glue, but the
                // when/for-whom *decision* is shared (conversations.md § Where logic lives) —
                // the pure, unit-tested `fauna_conversations::MessageNotificationTracker`
                // decides which threads warrant a toast; `crate::notifications` fires the
                // freedesktop toast. The shared tracker is interior-`Mutex` (it also backs a
                // UniFFI object for the native apps), so a plain `&self` binding suffices.
                //
                // A FRESH tracker per attach, deliberately: the clear that closed the
                // previous channel is an identity change (or a test reset), so the
                // incoming identity's restored threads are not "new" and must seed
                // silently. Carrying the outgoing identity's seed across would toast
                // once per restored thread on every account switch.
                let notif_tracker = fauna_conversations::MessageNotificationTracker::new();
                loop {
                    let pass = main_loop_meter::dispatch_guard("tray-toast-loop");
                    // The diff tick's start barrier — bumped BEFORE the snapshot
                    // read, so a negative e2e assertion ("the open thread raises no
                    // banner") can prove a tick that began after its plant has
                    // finished (`fauna_e2e_agent::MESSAGE_BANNERS_KEY`). A no-op in
                    // a release build.
                    fauna_conversations::banner_pass_started();
                    let snapshot = crate::conversations::manager().snapshot();
                    // The shared fold over EVERY thread (`ConversationsManager::
                    // unread_total`, lifted from this crate 2026-09-26) — not
                    // `snapshot.threads`, which a typed search narrows: the
                    // widget outside the app reports the account's unread.
                    let count = crate::conversations::manager().unread_total();
                    *crate::tray::tray_state()
                        .unread_count
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = count;
                    // The same number on linux's home-screen widget — the
                    // launcher badge (`launcher_badge.rs`; `linux.md`
                    // § Home-screen widget). One count, two surfaces.
                    crate::launcher_badge::publish_unread(count);
                    // Fire a toast for each thread with a new inbound message (the
                    // selected thread is suppressed inside the tracker — it is one of
                    // the three rules the shared decision owns). `label` is the DM
                    // peer / group name; `snippet` is the message preview. Each fire
                    // is recorded on the shared banner log, which is what the e2e
                    // witness for `conversations` outcome 11 reads; the record sits
                    // here rather than inside the tracker because it must mean "the
                    // user was shown this", not "the tracker returned this".
                    let activities: Vec<_> = snapshot
                        .threads
                        .iter()
                        .map(fauna_conversations::ThreadActivity::from_summary)
                        .collect();
                    for act in notif_tracker.diff(
                        activities,
                        snapshot.selected_thread_id.clone(),
                        snapshot.launch_floor_ms,
                    ) {
                        crate::notifications::notify_message(&act.label, &act.snippet);
                        fauna_conversations::record_fired_banner(act.thread_id, act.label);
                    }
                    fauna_conversations::banner_pass_completed();
                    drop(pass);
                    if conv_rx.recv().await.is_err() {
                        // The observer this channel belonged to was cleared — go
                        // round the outer loop and register a fresh one (with a
                        // fresh tracker) rather than leaving the app toast-less
                        // and its tray badge frozen for the rest of the process.
                        break;
                    }
                }
            }
        });
    });

    application.connect_activate(build_ui);

    // Ctrl+Q — force-quit regardless of close-to-tray setting.
    application.set_accels_for_action("app.quit", &["<Control>q"]);
    let quit_action = gio::SimpleAction::new("quit", None);
    let app_for_quit = application.clone();
    quit_action.connect_activate(move |_, _| {
        app_for_quit.quit();
    });
    application.add_action(&quit_action);

    // The add-account intent a colliding instance forwards here
    // (`instance_remote::forward_add_account` → `ActivateAction`). The wizard
    // belongs to whichever instance owns the onboarding scratchpad, so the
    // colliding process hands the intent over rather than running it: this is
    // the receiving half of that hand-off, and it is also why the action is
    // registered on EVERY launch, not only colliding ones — the receiver is
    // by definition the process that did *not* collide.
    let add_account_action = gio::SimpleAction::new("add-account", None);
    let app_for_add = application.clone();
    add_account_action.connect_activate(move |_, _| {
        // Raise ourselves first: the forwarding instance is exiting, so
        // without this the wizard could open behind another window.
        if let Some(win) = app_for_add
            .active_window()
            .or_else(|| app_for_add.windows().into_iter().next())
        {
            show_window(&win);
        }
        launch_add_account_wizard(&app_for_add);
    });
    application.add_action(&add_account_action);

    // Hand GTK an argv with `--autostart` filtered out. GApplication parses
    // argv itself (no HANDLES_COMMAND_LINE, no registered main options), so an
    // unrecognised option makes it print "Unknown option --autostart" and
    // refuse to start — which would break *every* desktop sign-in, since the
    // entry we register execs exactly that. `autostart::is_autostart_launch()`
    // reads the real process argv via `env::args()` and is unaffected by this
    // filtering, so the flag still does its job. Registering it as a real
    // GOption would also work, but the flag is OS wiring rather than a
    // user-facing switch, so it stays out of `--help`.
    application.run_with_args(&autostart::strip_autostart_flag(std::env::args()));
}

fn load_css() {
    let css = gtk::CssProvider::new();
    css.load_from_string(include_str!("style.css"));

    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("Could not get default display"),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

// Bearer-authed REST surface to our nest — a thin re-export of the shared
// `fauna-nest-http` crate (the `NestContentApi` trait + `ReqwestNestContentApi`
// 401-reactive refresh + `ApiError` + path constants) plus the
// LaunchMachine→BearerSource wiring. See the module docs (design tracked
// internally).
mod nest_content_api;

/// True when the app runs under e2e automation — either the legacy AT-SPI
/// bridge (`FAUNA_E2E_BRIDGE`) or the in-process automation agent
/// (`FAUNA_E2E_AGENT_PORT`). Both drive the *same* test-mode behaviour (mock
/// rail backends, deferred real FaunaMls backend, focus-free window mapping,
/// `NON_UNIQUE` GApplication), so every site that gates on "are we under e2e?"
/// must use this — gating on the bridge flag alone makes agent runs diverge
/// from bridge runs (e.g. Smtp/Bluesky inject fails "not implemented" because
/// no mock backend was installed). The agent is replacing AT-SPI; until that
/// cutover completes, both flags must mean identical app behaviour.
///
/// Gated as a pair with the production twin below (convention 15), the same
/// shape tui's crate-level `e2e_mode_enabled` carries. The twin is behaviour-preserving
/// by construction — a shipped app has neither variable set, so it already
/// answered `false` — but a release binary now neither compiles the reads nor
/// names them, which is the boundary the runtime check never was.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn e2e_mode_enabled() -> bool {
    std::env::var_os("FAUNA_E2E_BRIDGE").is_some()
        || std::env::var_os("FAUNA_E2E_AGENT_PORT").is_some()
}

/// Production twin: a shipped app is never under e2e automation.
#[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
pub fn e2e_mode_enabled() -> bool {
    false
}

/// Show a top-level window. For real users this maps **and** raises+focuses it
/// (`present`). Under e2e automation it only *maps* the window
/// (`set_visible(true)`) and skips `present`: the automation actuates the GTK4
/// app entirely coordinate-free and never needs the window focused, so every
/// per-test app launch would otherwise steal focus from whatever the developer
/// is doing on the shared session. Mapping alone exposes the full widget tree;
/// skipping `present` means no activation request, so gnome's
/// focus-stealing-prevention leaves the active window alone.
/// Launch the onboarding wizard in append ("Add account") mode from the running
/// authenticated session. Presented like any other onboarding window; on
/// success the wizard registers the new identity and switches to it (see
/// `views::onboarding::build_onboarding_window_append` + `register_switch_account_handler`).
/// Cancelling (X) just dismisses the wizard — the append-mode close handler
/// does not quit the app.
pub(crate) fn launch_add_account_wizard(app: &adw::Application) {
    let onboarding = views::onboarding::build_onboarding_window_append(app);
    // Point the E2E test agent's onboarding-machine cell at THIS wizard's machine
    // so `call_machine_method` (the handle-check / invite snapshot injectors) drive
    // the append wizard rather than the stale startup machine. Inert for real users
    // (the agent isn't wired). Mirrors the machine hand-off the real-onboarding
    // transition does via `current_onboarding_machine`.
    set_active_onboarding_machine(onboarding.machine.clone());
    show_window(&onboarding.window);
}

fn show_window<W: IsA<gtk::Window> + IsA<gtk::Widget>>(win: &W) {
    win.set_visible(true);
    if !e2e_mode_enabled() {
        // If the tray host handed us a fresh xdg-activation token (close-to-tray
        // "Open Fauna" on Wayland), feed it to GTK before presenting. GTK >= 4.14.6
        // consumes `set_startup_id` as the Wayland activation token, so mutter
        // honours this `present()` (raises+focuses the window) instead of denying
        // the raise under focus-stealing prevention and posting the passive
        // "Fauna is ready" notification. No token (normal launch / X11) → present()
        // behaves as before. See tray.rs `ACTIVATION_TOKEN` + libs/ksni/PATCH.md.
        if let Some(token) = crate::tray::take_activation_token() {
            win.set_startup_id(&token);
        }
        win.present();
    }
}

/// How long an `--autostart` launch waits for ksni to answer the tray-host
/// question before giving up and showing the window. Generous: the cost of
/// waiting is a slightly later window on a desktop nobody is watching yet,
/// while the cost of guessing wrong is a window stranded with no way back.
const TRAY_HOST_ANSWER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Present the authenticated main window — unless this is an auto-started
/// launch that should stay tray-resident (`apps/linux.md` § Auto-start at
/// sign-in).
///
/// **Why this is a poll and not an `if`.** The hidden-launch decision needs to
/// know whether a tray host exists, but `tray::TRAY_HOST_AVAILABLE` is stored
/// *asynchronously* by ksni's `watcher_online`/`watcher_offine` hooks and
/// defaults `false` — reading it here would say "no host" on every launch,
/// however many hosts are running. So we wait for `TRAY_HOST_ANSWERED` on the
/// existing tray→GTK poll pattern rather than block the GTK thread.
///
/// Both failure directions land on *visible*, which is always recoverable:
/// answered-no shows immediately, and an answer that never arrives (ksni only
/// calls the hooks on `Ok` and on `ServiceUnknown` — any other D-Bus error
/// answers neither way) shows at the timeout.
fn show_main_window_unless_autostart_hidden<W>(win: &W)
where
    W: IsA<gtk::Window> + IsA<gtk::Widget> + Clone + 'static,
{
    if !autostart::is_autostart_launch() {
        show_window(win);
        return;
    }

    // Already answered (the tray thread starts before GTK, so this is the
    // common case) — decide now, no poll needed.
    if tray::TRAY_HOST_ANSWERED.load(Ordering::SeqCst) {
        if !autostart::should_start_hidden(true, tray::tray_host_available()) {
            show_window(win);
        }
        return;
    }

    let win = win.clone();
    let deadline = std::time::Instant::now() + TRAY_HOST_ANSWER_TIMEOUT;
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        let answered = tray::TRAY_HOST_ANSWERED.load(Ordering::SeqCst);
        if !answered && std::time::Instant::now() < deadline {
            return glib::ControlFlow::Continue;
        }
        // Answered, or we waited long enough. `should_start_hidden` reads
        // `false` for host-availability in the timeout case, so an unanswered
        // question shows the window.
        if !autostart::should_start_hidden(true, answered && tray::tray_host_available()) {
            tracing::info!(
                answered,
                "[autostart] no confirmed tray host — presenting the window instead of hiding"
            );
            show_window(&win);
        } else {
            tracing::info!("[autostart] tray host confirmed — starting tray-resident (hidden)");
        }
        glib::ControlFlow::Break
    });
}

/// Did this plain launch collide with a live instance of the account it would
/// open? Answered once in `main()`, before GTK, and remembered here for
/// `build_ui` (which GApplication calls, so it takes no arguments of ours).
static LAUNCH_COLLIDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The collision test (`account-scoping.md` § Concurrent instances). Three
/// conditions, all required:
///
/// 1. **Plain launch.** A `FAUNA_BOUND_ACCOUNT` launch that collides stays
///    terminally refused in `app.rs` — "the chooser is strictly a human
///    affordance: wired IPC must be deterministic". Checked first, so a bound
///    launch never even probes.
/// 2. **A resolvable would-be account.** The store-active account is what a
///    plain launch binds to. `None` — a fresh install, or an index not yet
///    materialized — means there is nothing to collide with *and* nothing to
///    offer, so the ordinary routing applies.
/// 3. **That account is currently served**, per the shared display-only probe.
///
/// Answering here rather than at `AuthSuccess` is deliberate: this is the last
/// moment before GApplication's uniqueness would redirect the process into the
/// running instance, and it is upstream of the silent challenge, so a collided
/// launch never authenticates as an account it cannot run as.
///
/// A `true` result is remembered in [`LAUNCH_COLLIDED`]; the acquire in
/// `app.rs` remains the arbiter for every launch that gets past here.
fn launch_collision_detected() -> bool {
    if fauna_client_accounts::requested_bound_account().is_some() {
        return false;
    }
    let (Some(base), Some(active)) = (
        account_scope::install_state_base(),
        account_scope::active_actor_id_hex(),
    ) else {
        return false;
    };
    let collided = fauna_client_accounts::AccountInstanceLock::is_served(&base, &active);
    if collided {
        tracing::info!(
            "[launch-collision] {active} is already served by a live instance — offering the chooser"
        );
        LAUNCH_COLLIDED.store(true, Ordering::SeqCst);
    }
    collided
}

/// Render the launch-collision chooser and wire its three exits. Returns
/// `false` when this launch did not collide, so `build_ui` falls through to
/// the ordinary four-case routing.
fn show_launch_instance_chooser(application: &adw::Application) -> bool {
    if !LAUNCH_COLLIDED.load(Ordering::SeqCst) {
        return false;
    }
    let registry = account_registry();
    let served = account_scope::active_actor_id_hex().unwrap_or_default();
    // The already-served account's label, from the same shared formatter the
    // switcher uses — the collision names an account the user recognises.
    let served_label = registry
        .list()
        .into_iter()
        .find(|e| e.actor_id == served)
        .map(|e| fauna_core::format::account_display_label(e.handle.as_deref(), &e.actor_id))
        .unwrap_or_else(|| fauna_core::format::account_display_label(None, &served));
    let choices = account_scope::choosable_accounts(&registry);

    let app_for_pick = application.clone();
    let view_holder: Rc<
        std::cell::RefCell<Option<views::launch_instance_chooser::InstanceChooserView>>,
    > = Rc::new(std::cell::RefCell::new(None));

    let holder_for_pick = view_holder.clone();
    let on_pick = move |actor_id: &str| {
        // `bind_account` is the single gate — it re-checks the account exists,
        // has a secret, and is not re-auth-flagged. The instance lock is
        // re-taken later, at `AuthSuccess`; between this click and that
        // acquire the account can still be lost to another process, which is
        // exactly why the probe is display-only.
        if let Err(e) = account_registry().bind_account(actor_id) {
            tracing::warn!("[launch-collision] bind_account({actor_id}) refused: {e}");
            if let Some(v) = holder_for_pick.borrow().as_ref() {
                (v.show_error)(crate::i18n::strings::onboarding::instance_chooser::ACCOUNT_TAKEN);
            }
            return;
        }
        // This process is now that account's bound instance — no third
        // process. Every launch read below (the four-case routing, the launch
        // machine, `has_awaiting_dns_slot`) resolves through the binding from
        // here on, because `launch_persistence()` reads it.
        fauna_client_accounts::bind_session_launch_to(actor_id);
        if let Some(v) = holder_for_pick.borrow_mut().take() {
            v.window.destroy();
        }
        LAUNCH_COLLIDED.store(false, Ordering::SeqCst);
        build_ui(&app_for_pick);
    };

    let holder_for_focus = view_holder.clone();
    let app_for_focus = application.clone();
    let served_for_focus = served.clone();
    let on_focus_existing = move || {
        // The shared decision (`fauna_client_accounts::resolve_focus_existing`):
        // raise over the *per-account* activation endpoint, not the app-wide
        // name (`account-scoping.md` § the per-(OS login, account) raise
        // channel) — the server we want may be plain or bound, and only the
        // app-wide channel ever distinguished them. An unanswered raise looks
        // the same whether the sibling is gone or unreachable, so the decision
        // re-probes the lock rather than guess.
        use fauna_client_accounts::FocusExistingOutcome;
        match fauna_client_accounts::resolve_focus_existing(
            &served_for_focus,
            instance_remote::raise_account_instance,
            account_scope::is_account_served,
        ) {
            FocusExistingOutcome::Raised => app_for_focus.quit(),
            FocusExistingOutcome::NoLongerServed => {
                // The sibling died between the probe that rendered this
                // chooser and the click. There is no collision any more, so the
                // honest answer is the launch this process was always trying
                // to make.
                tracing::info!(
                    "[raise-channel] {served_for_focus} is no longer served — continuing as a plain launch"
                );
                if let Some(v) = holder_for_focus.borrow_mut().take() {
                    v.window.destroy();
                }
                LAUNCH_COLLIDED.store(false, Ordering::SeqCst);
                build_ui(&app_for_focus);
            }
            // Still served, but by a client that claims no endpoint (tui) or
            // over a bus we cannot reach. Do NOT quit: the user would be left
            // with no window and no explanation.
            FocusExistingOutcome::StillServedNoChannel => {
                if let Some(v) = holder_for_focus.borrow().as_ref() {
                    (v.show_error)(
                        crate::i18n::strings::onboarding::instance_chooser::NO_RUNNING_INSTANCE,
                    );
                }
            }
        }
    };

    let holder_for_add = view_holder.clone();
    let app_for_add = application.clone();
    let on_add_account = move || {
        // The add-account forward stays on the APP-WIDE channel: the wizard
        // belongs to the primary, and only a plain instance owns that name.
        if instance_remote::name_has_owner(APP_ID) {
            if instance_remote::forward_add_account(APP_ID) {
                app_for_add.quit();
                return;
            }
            if let Some(v) = holder_for_add.borrow().as_ref() {
                (v.show_error)(
                    crate::i18n::strings::onboarding::instance_chooser::NO_RUNNING_INSTANCE,
                );
            }
            return;
        }
        // Nobody owns the app-wide name, so the instance we collided with is
        // *bound* — and this plain process is at that moment the install's
        // only plain instance, hence the primary. It runs the wizard itself
        // rather than forwarding into nothing (`account-scoping.md`: the
        // no-owner rule that closes the forward's last open case).
        tracing::info!("[launch-collision] no primary owns {APP_ID} — running the wizard here");
        if let Some(v) = holder_for_add.borrow_mut().take() {
            v.window.destroy();
        }
        LAUNCH_COLLIDED.store(false, Ordering::SeqCst);
        launch_add_account_wizard(&app_for_add);
    };

    let view = views::launch_instance_chooser::build_instance_chooser_window(
        application,
        &served_label,
        &choices,
        on_pick,
        on_focus_existing,
        on_add_account,
    );
    start_test_agent_if_enabled(
        application,
        None,
        None,
        Some(view.error_label.clone()),
        None,
        None,
        None,
        None,
        None,
    );
    show_window(&view.window);
    *view_holder.borrow_mut() = Some(view);
    true
}

fn build_ui(application: &adw::Application) {
    // If this is a re-activation (e.g. D-Bus activate, second launch, or
    // tray/notification raise), present an existing window instead of
    // building a new one. `active_window()` alone isn't enough — it
    // returns None when no window currently has focus, and without this
    // guard a notification close during a new user's first session would
    // trigger a second `launch_authenticated` and spawn a duplicate main
    // window that couldn't be closed.
    if let Some(existing) = application
        .active_window()
        .or_else(|| application.windows().into_iter().next())
    {
        show_window(&existing);
        return;
    }

    // The launch-collision chooser preempts the four-case routing below: this
    // process cannot become the account that routing would resolve, so it must
    // not run the silent challenge for it (account-scoping.md § Concurrent
    // instances). Cleared once the user picks, at which point `build_ui`
    // re-enters and routes on the *bound* account.
    if show_launch_instance_chooser(application) {
        return;
    }

    // The region content plane: the device record is restored before any
    // surface renders and before the first relay fetch (`region-blocking.md`
    // § Fail posture — loaded at launch ahead of the first fetch).
    region::launch();

    register_sign_out_handler(application);
    register_factory_reset_handler(application);
    register_switch_account_handler(application);
    register_launch_escalation_handler(application);

    // No boot re-mirror any more (2026-09-24): the five-case routing below
    // reads the registry's ACTIVE account, and an append-mode wizard writes
    // nothing until its own terminal registers and switches
    // (`fauna_client_accounts::persist_confirmed_identity`'s append rule), so
    // there is no single slot an abandoned append could have polluted.

    // The five-case launch routing lives in `classify_launch`, shared with the
    // account switch's rebuild (which must land a target on the SAME surface a
    // relaunch would show). A cold start owns the drain, so its surface starts
    // it, and Case 1 runs the full silent-challenge flow.
    let route =
        stored_launch_route(launch_credentials().map(|(url, secret, _device)| (url, secret)));
    present_launch_route(
        application,
        route,
        AgentWiring::ColdStart,
        |node_url, secret_hex| {
            // Case 1: full identity + nest_url → launch screen + silent challenge,
            // then route per the four documented outcomes (Authenticated /
            // Unregistered / Transient / Unreachable) per
            // docs/goal/behavior/onboarding.md "App-launch routing".
            launch_silent_challenge_flow(application, node_url, secret_hex);
        },
    );
}

/// Where a stored account state lands — the app-launch routing
/// (`onboarding.md` § App-launch routing), one decision shared by the cold
/// start (`build_ui`) and the account switch's rebuild
/// (`register_switch_account_handler`). A switch lands its target on exactly
/// the surface a relaunch would show — which for the append + pending-invite
/// adoption is the wizard at `invite_request` (`onboarding.md` § Multi-account),
/// not an authenticated window over a nest the account has no `nest_url` for.
///
/// The variants are in `LaunchMachine::start()`'s own row order.
#[derive(Debug, PartialEq)]
enum LaunchRoute {
    /// Outranks every row below, exactly as `LaunchMachine::start()`'s own
    /// `account_index_refusal` check does (checked before it even attempts
    /// `load_identity`). Needed here too because `classify_launch`'s
    /// `credentials` param comes from `AccountRegistry::active()`, which by
    /// design reports nothing for an unreadable/malformed index
    /// (`AccountRegistry::index()`'s `Unreadable`/`Malformed` arms return
    /// `AccountIndex::default()` rather than lying with a guessed shape) —
    /// so a caller that only reads `active()` cannot tell "no accounts" from
    /// "accounts this build cannot see", and fell through to `Fresh`,
    /// skipping the refusal screen entirely and offering a new identity over
    /// one sitting intact behind a blob this build merely cannot parse.
    IndexRefused,
    /// Case 0 — the deferred-DNS row, and it MUST be tested before case 1: while
    /// the records are not yet at the registrar the nest is unreachable by
    /// definition, so a silent challenge could only fail through to the retry
    /// surface. It cannot ride case 1 either: the deferred-DNS exit deliberately
    /// persists NO nest_url (the nest is not claimed yet), so without this row
    /// the state falls into case 3 and silently discards the half-provisioned
    /// nest the user just paid for. The routing DECISION still belongs to the
    /// shared machine: the silent-challenge flow runs with an empty `node_url`
    /// (the record carries its own).
    AwaitingDns { secret_hex: String },
    /// Case 1 — full identity + nest_url → silent challenge → main app. There is
    /// no storage mode to owe (every nest is sealed from first boot), so there is
    /// no wizard-resume branch off this case.
    Authenticated {
        node_url: String,
        secret_hex: String,
    },
    /// Case 2 — identity, no nest_url, pending invite → seed both, wizard at
    /// InviteRequest.
    PendingInvite {
        secret_hex: String,
        record: fauna_launch_machine::PendingInviteRecord,
    },
    /// Case 3 — identity only → seed identity, wizard at HandleEntry.
    HandleEntry { secret_hex: String },
    /// Case 4 — nothing → fresh wizard at IdentityChoice.
    Fresh,
}

/// The pure routing decision over `(node_url, secret_hex)` and the two slot
/// reads it may need. The reads are closures so each runs only when its row is
/// reached, exactly as the inline match this replaced read them.
fn classify_launch(
    index_refusal: bool,
    credentials: Option<(String, String)>,
    has_awaiting_dns: impl FnOnce() -> bool,
    load_pending_invite: impl FnOnce() -> Option<fauna_launch_machine::PendingInviteRecord>,
) -> LaunchRoute {
    if index_refusal {
        return LaunchRoute::IndexRefused;
    }
    let Some((node_url, secret_hex)) = credentials.filter(|(_, secret)| !secret.is_empty()) else {
        return LaunchRoute::Fresh;
    };
    if has_awaiting_dns() {
        return LaunchRoute::AwaitingDns { secret_hex };
    }
    if !node_url.is_empty() {
        return LaunchRoute::Authenticated {
            node_url,
            secret_hex,
        };
    }
    match load_pending_invite() {
        Some(record) => LaunchRoute::PendingInvite { secret_hex, record },
        None => LaunchRoute::HandleEntry { secret_hex },
    }
}

/// [`classify_launch`] over the ACTIVE account's stored slots. Both slot reads go
/// through the `RegistryLaunchPersistence` adapter `LaunchMachine::start()`
/// branches on — no bespoke store API, no second source of truth — so they
/// honor `FAUNA_E2E_CREDENTIAL_DIR` headless.
fn stored_launch_route(credentials: Option<(String, String)>) -> LaunchRoute {
    let index_refusal = account_registry().index_refusal().is_some();
    classify_launch(index_refusal, credentials, has_awaiting_dns_slot, || {
        use fauna_launch_machine::LaunchPersistence as _;
        launch_persistence().load_pending_invite()
    })
}

/// How a surface [`present_launch_route`] builds reaches the e2e test agent.
/// Inert for real users either way (the agent isn't wired).
enum AgentWiring {
    /// The process's first surface: start the drain over it.
    ColdStart,
    /// A surface replacing a torn-down session (the account switch): the first
    /// launch's drain is still running, so re-point its cells at the new window
    /// — sign-out's shape — and never start a second drain.
    Rebind,
}

/// Land the application on `route`'s surface. Case 1 is the caller's
/// (`on_authenticated`): a cold start runs the silent-challenge flow, the
/// account switch its in-session rebuild. Cases 0/2/3/4 are shared — the
/// deferred-DNS row enters the silent-challenge flow (which wires its own
/// surfaces, as the launch-escalation handler already relies on), and the
/// wizard rows build their wizard and hand it to the agent per `wiring`.
fn present_launch_route(
    application: &adw::Application,
    route: LaunchRoute,
    wiring: AgentWiring,
    on_authenticated: impl FnOnce(String, String),
) {
    match route {
        LaunchRoute::IndexRefused => {
            // No identity or nest_url is knowable from an index this build
            // cannot read — pass placeholders. `run_silent_challenge_async`'s
            // `LaunchMachine::start()` checks `account_index_refusal()`
            // unconditionally before it ever touches them, so the refusal
            // screen renders regardless of what is passed here.
            launch_silent_challenge_flow(application, String::new(), String::new());
        }
        LaunchRoute::AwaitingDns { secret_hex } => {
            fauna_client_accounts::refuse_if_bound_from_onboarding(
                "the awaiting-DNS onboarding resume",
            );
            launch_silent_challenge_flow(application, String::new(), secret_hex);
        }
        LaunchRoute::Authenticated {
            node_url,
            secret_hex,
        } => on_authenticated(node_url, secret_hex),
        LaunchRoute::PendingInvite { secret_hex, record } => {
            // Cases 2/3 route to the wizard — a bound launch refuses instead.
            fauna_client_accounts::refuse_if_bound_from_onboarding(
                "the onboarding wizard (invite/handle entry)",
            );
            let onboarding =
                views::onboarding::build_onboarding_window_with_seed(application, &secret_hex);
            onboarding.machine.seed_pending_invite(
                record.nest_url,
                record.handle,
                record.request_id,
                record.status_json,
            );
            show_launch_wizard(application, onboarding, wiring);
        }
        LaunchRoute::HandleEntry { secret_hex } => {
            fauna_client_accounts::refuse_if_bound_from_onboarding(
                "the onboarding wizard (invite/handle entry)",
            );
            let onboarding =
                views::onboarding::build_onboarding_window_with_seed(application, &secret_hex);
            show_launch_wizard(application, onboarding, wiring);
        }
        LaunchRoute::Fresh => {
            fauna_client_accounts::refuse_if_bound_from_onboarding("the fresh onboarding wizard");
            // A residue an earlier sign-out recorded is re-swept silently,
            // and painted on `identity_choice` only if something is still
            // left (`account-scoping.md` § Erasure follows scope → *the
            // residue surface*).
            account_scope::recheck_residue_at_launch();
            let onboarding = views::onboarding::build_onboarding_window(application);
            show_launch_wizard(application, onboarding, wiring);
        }
    }
}

/// Hand a launch-routed wizard to the e2e agent per `wiring`, then show it.
fn show_launch_wizard(
    application: &adw::Application,
    onboarding: views::onboarding::OnboardingResult,
    wiring: AgentWiring,
) {
    match wiring {
        AgentWiring::ColdStart => start_test_agent_if_enabled(
            application,
            None,
            None,
            Some(onboarding.error_label.clone()),
            None,
            None,
            None,
            Some(onboarding.machine.clone()),
            None,
        ),
        AgentWiring::Rebind => {
            // Forget the torn-down authenticated shell, or the state protocol
            // keeps reporting it (`retire_active_window`'s ⚠), and point the
            // label + machine cells at the wizard the user now sees.
            retire_active_window();
            set_active_error_label(onboarding.error_label.clone());
            set_active_onboarding_machine(onboarding.machine.clone());
        }
    }
    show_window(&onboarding.window);
}

/// The first, synchronous step of every user-facing session teardown (sign-out,
/// factory reset, account switch, identity change): stop the UI message pump
/// and make every window inert, BEFORE the account runtime's stop is handed off
/// the main thread (`actor_scope::reset_actor_scoped_state`).
///
/// The stop can take its whole budget (`ACCOUNT_RUNTIME_STOP_BUDGET`) when it
/// lands behind a prologue, and the main loop now runs meanwhile — so the
/// outgoing window stays on screen for that long. Inert, it cannot sign out a
/// second time or act as an account that is being stopped; pump-less, the
/// outgoing client can no longer repaint the actor-scoped state the reset has
/// just dropped. Neither is ordered against the stop: the pump carries the
/// client's UI messages, not the store's, and the stop's completion comes back
/// on its own glib future, never through it.
fn quiesce_for_teardown(app: &adw::Application) {
    settings::trigger_pump_shutdown();
    for win in app.windows() {
        win.set_sensitive(false);
    }
}

// Register the sign-out handler used by the Sign Out button in settings.
// Clears keyring credentials, closes the current window, and presents the
// onboarding wizard. Safe to call when no main window exists; in that case
// the active-window close is a no-op.
fn register_sign_out_handler(application: &adw::Application) {
    let app_for_sign_out = application.clone();
    settings::set_sign_out_handler(move || {
        // A sign-out is a session teardown — counted synchronously here, before
        // the deferral, for the same reason the account switch is
        // (`automation::link::record_session_teardown`). Gated: the `automation`
        // module itself is compiled out of release builds (e2e-conventions.md
        // § point 15), so every call site must be too, or `just linux-release`
        // fails to link.
        // The departing session's stolen-identity ceremony, if any, can no
        // longer own a supersession (`settings::stolen_hold`).
        settings::stolen_ceremony_teardown();
        #[cfg(any(debug_assertions, feature = "e2e-agent"))]
        automation::link::record_session_teardown();
        // Defer teardown so the dialog whose response handler triggered us
        // has time to fully unmap and release its modal grab. `idle_add`
        // alone fires too early — the dialog's close() is still queued, so
        // every event after present() goes to a dangling modal grab and the
        // new onboarding window appears frozen. A 100ms timeout reliably
        // outlasts the dismissal animation.
        let app = app_for_sign_out.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
            // Identity-scoped singletons survive the window teardown, so a
            // sign-out that skipped these would carry the outgoing identity's
            // threads and drafts into the next sign-in. One canonical list —
            // never hand-listed here (`actor_scope`).
            crate::conversations::manager().clear_for_identity_change();
            crate::conversations::manager().clear_observers();
            quiesce_for_teardown(&app);
            crate::actor_scope::reset_actor_scoped_state(
                fauna_client_account_runtime::StopReason::SignOut,
                move || sign_out_after_stop(&app),
            );
        });
    });
}

/// The half of a sign-out that must follow the account runtime's stop — every
/// line of it: the writers go down, then the erase, then the onboarding window
/// (`actor_scope::reset_actor_scoped_state`'s `then`).
fn sign_out_after_stop(app: &adw::Application) {
    // The external sync agent is already un-provisioned (it deleted its
    // persisted capability and stopped engines): its reply is part of the stop
    // this continuation waited for (`account_runtime::teardown`). A plain app
    // quit deliberately does not un-provision.
    tray::SIGNING_OUT.store(true, Ordering::SeqCst);
    // Clear any queued tray-compose request — the compose dialog
    // requires an authenticated FaunaClient, and we're tearing
    // that client down. A stale `true` here would spuriously pop
    // the compose dialog on the next sign-in.
    tray::TRAY_COMPOSE.store(false, Ordering::SeqCst);

    // Close every application-registered window: the main window
    // (Settings lives in-window, in the same shell — there is no
    // separate `PreferencesWindow`) and any compose/lightbox windows
    // that were registered with the application. `crate::confirm_dialog`
    // confirms (`adw::AlertDialog`) are embedded in their host
    // window's own widget tree and close with it automatically; the
    // handful of sites still on a separate-toplevel `adw::MessageDialog`
    // (rename/add-participant, the feed train-target sheet, the
    // folder share dialog) rely on GTK's transient-parent destroy
    // propagation instead. The fresh onboarding window is built
    // *after* this loop, so it can't accidentally be in
    // `app.windows()` yet.
    for win in app.windows() {
        win.close();
    }

    // Erase LAST: the writers are down, so nothing can land a credential
    // back into the namespace after the sweep passes over it
    // (`long-term-store.md` § Cleanup contract — "drop the launch
    // machine, the client, and any registry reader first; erase last").
    // Erase before building the onboarding window, which constructs a
    // fresh registry.
    //
    // Account-scoped local state erases too — the sign-out "all
    // accounts" half of account-scoping.md's "Erasure follows scope"
    // corollary — and MUST run before `delete_credentials` wipes the
    // registry this reads to know which actors existed.
    // The erase is best-effort BY DESIGN — a sign-out completes even
    // when a scope or a credential will not go — but proceeding must not
    // look identical to succeeding: what survived goes to the log by
    // path and key name, and to the user as one line, on the onboarding
    // window this hands them (`account-scoping.md` § Erasure follows
    // scope; `principles.md` § The user always controls their data puts
    // the delete affordance in the app, and a log is not one).
    //
    // ⚠ The line is built after BOTH halves. It used to be built from
    // the filesystem erase alone, one line above a `let _ =` on the
    // credential wipe — so a keyring that refused the wipe painted a
    // clean "Signed out" over the identity seed it still held.
    let survivors = account_scope::erase_all_known_accounts();
    let credentials = client::delete_credentials();
    // Recorded (install-scoped, so it outlives this process) BEFORE the
    // wizard is built: `identity_choice` paints its `sign-out-residue` view
    // from that state on every tick, never from a line written once onto a
    // banner the wizard's own render then wipes.
    account_scope::record_residue(&survivors, &credentials);

    let onboarding = views::onboarding::build_onboarding_window(app);
    // Hand the automation surface the window the USER is now looking at.
    // Both cells, and neither is optional: without the label one, the
    // state protocol keeps serializing the closed window's label — which
    // is how the residue line above could be on screen and read back as
    // `''`; without the machine one, the handle-check injectors drive a
    // wizard nobody can see. Inert for real users.
    // And forget the shell this handler just closed, or the next
    // session patch finds a "mounted" stack and never builds a window
    // (`retire_active_window`'s ⚠).
    retire_active_window();
    set_active_error_label(onboarding.error_label.clone());
    set_active_onboarding_machine(onboarding.machine);
    show_window(&onboarding.window);
}

/// Register the post-auth launch-escalation handler (`security.md`
/// § Post-auth surfacing, ratified 2026-07-23).
///
/// A mid-session **escalating** verdict — today from the background silent
/// challenge (`client.rs`'s `silent_sign_in`), tomorrow also from the hourly
/// bearer re-mint once the shared taxonomy carries the typed variant — routes
/// to the **same blocking surface the launch path renders** for it, with no
/// retry CTA and no softer per-app shape.
///
/// **Two verdicts, one handler.** `IdentityChanged` (the nest's pinned identity
/// changed) and `Superseded` (this identity was succeeded —
/// `identity-succession.md` § Propagation → *Own device fleet*) share this
/// teardown verbatim, because it is verdict-agnostic: both sessions are already
/// de-facto dead, both keep credentials, and it is the re-entered launch flow
/// — not this function — that re-derives which surface to land on (the re-trust
/// surface, or the identity-import screen). A second handler would be this one
/// copied, and copies drift.
///
/// **Why this re-enters the real launch flow rather than painting the surface
/// in place.** The surface's re-trust button drives
/// `LaunchMachine::trust_nest_identity()` on *the machine that produced the
/// verdict* — it reads the secret + nest_url off that state, and is a no-op on
/// any machine not sitting in `IdentityChanged`. A synthesized phase over a
/// machine-less launch view would render an identical-looking surface whose
/// trust button silently did nothing: the dead-button class the launch path
/// already paid for once (the pre-2026-07-13 dead Retry). Re-running the
/// challenge costs one round trip and yields a live machine, and if the nest
/// has meanwhile reverted to the pinned identity the user is simply let back
/// in — the honest answer, not a special case.
///
/// The teardown mirrors sign-out's, with one deliberate difference:
/// **credentials are NOT erased**. Nothing is wrong with the user's secret —
/// the *nest* changed, or the *account* moved — so re-trusting, picking a
/// different nest, and importing a successor must all still have the identity
/// in hand.
fn register_launch_escalation_handler(application: &adw::Application) {
    let app_for_identity = application.clone();
    // Re-entrancy guard: the verdict can arrive from more than one background
    // refresh in flight at once, and each would otherwise tear down the window
    // the previous one just rebuilt. First verdict wins; the flag never clears,
    // because the session it guards is over either way — the user leaves this
    // surface through the launch flow's own exits.
    let blocking = std::rc::Rc::new(std::cell::Cell::new(false));
    settings::set_launch_escalation_handler(move || {
        if blocking.replace(true) {
            return;
        }
        // Count the teardown HERE — synchronously, before the 100 ms deferral
        // below, same as the sign-out/switch/factory-reset sites
        // (`automation::link::record_session_teardown` documents why the
        // deferred closure is too late for a barrier that is a glib idle).
        // This mid-session escalation is a session teardown exactly like
        // those three and was the one arm that skipped it (tui's identical finding on the same class of arm).
        // The departing session's stolen-identity ceremony, if any, can no
        // longer own a supersession (`settings::stolen_hold`).
        settings::stolen_ceremony_teardown();
        #[cfg(any(debug_assertions, feature = "e2e-agent"))]
        automation::link::record_session_teardown();
        let app = app_for_identity.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
            // Read the session's nest + identity BEFORE teardown — the launch
            // flow needs both, and the client we are about to drop is where
            // they hang off.
            let (node_url, secret_hex) = match settings::get_client() {
                Some(c) => (c.node_url().to_string(), c.secret_hex().to_string()),
                None => {
                    tracing::error!(
                        "[identity] identity-changed verdict with no live client — cannot \
                         re-enter the launch flow"
                    );
                    return;
                }
            };

            // Drop the session. Same order as the account switch: stop the
            // pump and the background services first, DESTROY every window so
            // the widget tree releases its FaunaClient clones, then shut the
            // outgoing client's runtime down explicitly (Drop never fires — a
            // GTK signal-closure cycle keeps its Rc clones alive). Shutting the
            // client down is also what drops the bearer the goal doc requires
            // dropped: it takes the connections that carry it with it.
            let outgoing_client = settings::get_client();
            // Identity-scoped singleton: both verdicts mean the outgoing
            // session's identity is no longer valid here, and re-entering the
            // launch flow may land on a DIFFERENT successor identity — so a
            // skipped wipe would carry the outgoing identity's threads and
            // drafts into it.
            crate::conversations::manager().clear_for_identity_change();
            crate::conversations::manager().clear_observers();
            quiesce_for_teardown(&app);
            crate::actor_scope::reset_actor_scoped_state(
                fauna_client_account_runtime::StopReason::AccountSwitch,
                move || {
                    // The agent is already un-provisioned, as on an account switch
                    // (it held a capability for a nest we can no longer trust): its
                    // reply is part of the stop this waited for.
                    tray::TRAY_COMPOSE.store(false, Ordering::SeqCst);
                    for win in app.windows() {
                        win.destroy();
                    }
                    if let Some(c) = outgoing_client {
                        c.shutdown();
                    }
                    // destroy() bypasses connect_close_request, so no SIGNING_OUT dance
                    // is needed; clear it so the launch window's close behaves normally.
                    tray::SIGNING_OUT.store(false, Ordering::SeqCst);

                    // Re-enter the launch flow. The challenge re-runs and earns the
                    // same refusal, so it lands on the phase that verdict owns —
                    // `IdentityChanged` with a live machine behind the re-trust button,
                    // or the terminal `Superseded` state on the identity-import screen
                    // — with one code path shared with a cold start.
                    launch_silent_challenge_flow(&app, node_url, secret_hex);
                },
            );
        });
    });
}

/// Register the factory-reset handler. The same deferred teardown as sign-out
/// (so the confirm dialog's modal grab is released before we tear down), but
/// instead of `delete_credentials` it **keeps** the local identity — the nest
/// was wiped, not the client, so the admin re-claims with the same secret — and
/// re-seeds onboarding at the claim-code step with the returned code pre-filled
/// (the human never sees it). The nest is mid-restart (~1-2s) when this fires;
/// the claim-code page's existing transient-error retry covers the window. Per
/// `docs/goal/behavior/mail-bridge-lifecycle.md` § Factory reset.
fn register_factory_reset_handler(application: &adw::Application) {
    let app_for_reset = application.clone();
    settings::set_factory_reset_handler(move |claim_code, nest_url, secret_hex, handle| {
        // A factory reset drops the authenticated session too — counted
        // synchronously here, before the deferral, for the same reason as the
        // switch and sign-out arms (`automation::link::record_session_teardown`).
        // The departing session's stolen-identity ceremony, if any, can no
        // longer own a supersession (`settings::stolen_hold`).
        settings::stolen_ceremony_teardown();
        #[cfg(any(debug_assertions, feature = "e2e-agent"))]
        automation::link::record_session_teardown();
        let app = app_for_reset.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
            // Drop the authenticated session but DO NOT delete credentials —
            // the identity stays valid for the re-claim against the fresh nest.
            // The session's in-memory state still goes: the nest it described is
            // being replaced, so carrying it into the re-claim is the same
            // wrong-actor render as on any other teardown (`actor_scope`).
            quiesce_for_teardown(&app);
            crate::actor_scope::reset_actor_scoped_state(
                fauna_client_account_runtime::StopReason::AccountSwitch,
                move || {
                    tray::SIGNING_OUT.store(true, Ordering::SeqCst);
                    tray::TRAY_COMPOSE.store(false, Ordering::SeqCst);

                    for win in app.windows() {
                        win.close();
                    }

                    let onboarding =
                        views::onboarding::build_onboarding_window_with_seed(&app, &secret_hex);
                    onboarding
                        .machine
                        .navigate_to_claim_code_for_known_nest_with_code(
                            nest_url, handle, claim_code,
                        );
                    start_test_agent_if_enabled(
                        &app,
                        None,
                        None,
                        Some(onboarding.error_label.clone()),
                        None,
                        None,
                        None,
                        Some(onboarding.machine.clone()),
                        None,
                    );
                    show_window(&onboarding.window);
                },
            );
        });
    });
}

// Register the account-switch handler used by the account-switcher rows in
// settings. Switching to another held identity is a live in-session re-auth:
// make the target account active, tear down the current authenticated window +
// client, then rebuild the authenticated session for the now-active account —
// no relaunch (Decision 1, switch-first; tracked internally;
// `long-term-store.md` § Shared seam: "switching
// accounts is `set_active` + a client teardown/rebuild of the launch machine").
// This is also the multi-account home of the e2e "set_state can't re-auth a
// running client" gap — the switch IS an in-session re-auth. Captures only the
// `adw::Application` (like `register_sign_out_handler`); the teardown reaches the
// live client via `settings::get_client()` and the rebuild registers the new one
// via `launch_authenticated` → `settings::set_client` — or, for a target that is
// not yet connectable (a pending-invite append), lands the surface a relaunch
// would show for it, through the same `classify_launch` routing `build_ui` runs.
fn register_switch_account_handler(application: &adw::Application) {
    let app_for_switch = application.clone();
    // Re-entrancy guard: the switcher wires the switch on BOTH the row's
    // `activated` signal AND the parent listbox's `row-activated` (the e2e agent
    // actuates a `ListBoxRow` via the latter, real users via the former), so one
    // click can fire the handler twice. It also stops a double-tap on two rows
    // from racing two teardown/rebuilds. First trigger wins; the flag clears once
    // the rebuild completes.
    let switch_pending = std::rc::Rc::new(std::cell::Cell::new(false));
    settings::set_switch_account_handler(move |actor_id: String, confirmed: bool| {
        if switch_pending.replace(true) {
            return; // a switch is already in flight; ignore the duplicate.
        }
        // 1. Read the target account's slots and make it active — synchronously,
        //    BEFORE the teardown is counted or scheduled, so a refusal is exactly
        //    "nothing happened": the shared line on the Account page, the live
        //    session untouched, no teardown recorded (the witness,
        //    `test_linux_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused`,
        //    anchors on `session_generation` not moving). A fresh registry over
        //    the same File/libsecret backend — cheap, reads the store directly.
        let registry = account_registry();
        let Some(stored) = registry.secrets(&actor_id) else {
            settings::account::paint_switch_refusal(
                &actor_id,
                &fauna_client_accounts::AccountError::NoStoredSecret(actor_id.clone()),
            );
            switch_pending.set(false);
            return;
        };
        let node_url = stored.nest_url.clone().unwrap_or_default();
        let secret_hex = stored.secret_hex.clone();
        // `confirmed` is the Stage-2 re-auth bit (`long-term-store.md`
        // § Multi-account evolution): true iff the in-app re-auth prompt
        // (`settings::account::request_switch_account`) just succeeded, which
        // routes through `set_active_confirmed`. Plain `set_active` REFUSES a
        // flagged account with `ConfirmationRequired`, so a path that skipped
        // the prompt fails loudly right here — nothing has been torn down yet,
        // so the live session is untouched.
        let activated = if confirmed {
            registry.set_active_confirmed(&actor_id)
        } else {
            registry.set_active(&actor_id)
        };
        if let Err(e) = activated {
            settings::account::paint_switch_refusal(&actor_id, &e);
            switch_pending.set(false);
            return;
        }
        // Count the teardown HERE — synchronously, before the 100 ms deferral
        // below (`automation::link::record_session_teardown` documents why the
        // deferred closure is too late for a barrier that is a glib idle) — and
        // only once the switch is certain to happen. After the re-entrancy
        // guard, so the ignored duplicate trigger does not count a second
        // teardown that never happens.
        // The departing session's stolen-identity ceremony, if any, can no
        // longer own a supersession (`settings::stolen_hold`).
        settings::stolen_ceremony_teardown();
        #[cfg(any(debug_assertions, feature = "e2e-agent"))]
        {
            automation::link::record_session_teardown();
            automation::link::adopt_switched_session(&node_url, &secret_hex);
        }
        let app = app_for_switch.clone();
        let pending = switch_pending.clone();
        // Defer teardown so the switcher row's click handling settles before we
        // destroy its window (mirrors the sign-out/factory-reset deferral).
        glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
            // The rebuilt status/switcher read the now-active account's cached
            // handle/domain/tier straight from the registry
            // (`client::load_account_cache`), so nothing else is written here.
            // The in-process cache mirror is reset so the incoming account never
            // inherits the outgoing session's `<handle>@<domain>`.
            client::reset_account_cache_mem();

            // 2. Tear down the current authenticated window + client. Mirror the
            //    test-agent reset arm: pump shutdown, backup cancel, clear the
            //    tray-compose latch, DESTROY (not close) every window so the
            //    widget tree drops its FaunaClient clones, then shut the outgoing
            //    client's runtime down explicitly (Drop never fires — a
            //    distributed GTK signal-closure cycle keeps its Rc clones alive).
            //    Grab a strong ref to the outgoing client BEFORE teardown so the
            //    shutdown is guaranteed even though `launch_authenticated` (below)
            //    will replace the settings client registration.
            //
            //    "Every window" includes an append ("Add account") wizard that
            //    triggered this switch — deliberately: its adoption lands a FRESH
            //    launch-routed surface for the new account in step 3, the same
            //    destroy-all-then-rebuild sign-out runs and apple/windows ship.
            //    The 100 ms deferral above means the wizard's own submit
            //    continuation has already returned.
            let outgoing_client = settings::get_client();
            // Identity-scoped singleton: survives the window teardown exactly
            // like critical_alerts/screen_lock below, so a switch that
            // skipped this would render the outgoing account's threads,
            // drafts and selection to the incoming one — the same leak the
            // test-agent's actor-switch patch already guards against.
            crate::conversations::manager().clear_for_identity_change();
            crate::conversations::manager().clear_observers();
            quiesce_for_teardown(&app);
            crate::actor_scope::reset_actor_scoped_state(
                fauna_client_account_runtime::StopReason::AccountSwitch,
                move || {
                    // The outgoing account's agent capability is already
                    // un-provisioned (part of the stop this waited for); the
                    // incoming account re-provisions at its post-auth.
                    tray::TRAY_COMPOSE.store(false, Ordering::SeqCst);
                    for win in app.windows() {
                        win.destroy();
                    }
                    if let Some(c) = outgoing_client {
                        c.shutdown();
                    }
                    // We destroy() (bypasses connect_close_request), so no SIGNING_OUT
                    // dance is needed for teardown; clear it so the incoming window's
                    // close handler behaves normally (honors the close-to-tray pref).
                    tray::SIGNING_OUT.store(false, Ordering::SeqCst);

                    // 3. Rebuild the now-active account's launch surface through the SAME
                    //    routing a relaunch runs (`classify_launch`), over the target's own
                    //    stored slots. A connectable account (Case 1) gets the in-session
                    //    authenticated rebuild below; any other lands where a relaunch
                    //    would — the append + pending-invite adoption's target has no
                    //    nest_url yet, so it lands on its wizard at `invite_request`
                    //    (`onboarding.md` § Multi-account), never on `launch_authenticated`
                    //    against an empty URL.
                    // The same plain-`String` hand-off `launch_credentials` makes for the
                    // cold start's read of the same slot.
                    let route =
                        stored_launch_route(Some((node_url, secret_hex.as_str().to_string())));
                    let rebuilds_async = matches!(route, LaunchRoute::Authenticated { .. });
                    let app_for_auth = app.clone();
                    let pending_for_auth = pending.clone();
                    present_launch_route(
                        &app,
                        route,
                        AgentWiring::Rebind,
                        move |node_url, secret_hex| {
                            let app = app_for_auth;
                            let pending = pending_for_auth;
                            //    Case 1: construct the LaunchMachine fresh and drive `start()` to Online
                            //    before FaunaClient consumes it. The registry adapter reads the
                            //    account we just made active.
                            //
                            //    **`start()` runs OFF the GTK main thread** (`run_on_tokio`);
                            //    the rebuild is its continuation. It used to be a `block_on` on
                            //    a runtime built and dropped *on this thread* — and this closure
                            //    is a `glib::timeout_add_local_once`, i.e. the GTK main loop, on
                            //    which the e2e agent's own 50 ms drain is another timeout source.
                            //    Same shape, same reason, as the post-wizard launch in
                            //    `views/onboarding/mod.rs`; the cold-launch path below has always
                            //    driven `LaunchMachine::start` this way.
                            //
                            //    ⚠ **This did NOT fix row 23** — do not
                            //    re-derive that. Row 23 named a stall here as its leading
                            //    candidate for "the append never switches the live session".
                            //    Measured 2026-08-16: the append never reaches this handler at
                            //    all. `handle_wizard_done`'s append branch fails first, in
                            //    `registry.add_account`, with `invalid secret: expected 32 bytes
                            //    (64 hex chars), got 0` — an empty secret out of
                            //    `client::load_credentials()` — and takes the `Err` arm that
                            //    dismisses the wizard, in a hot loop. The switcher e2e journeys
                            //    that DO drive this handler (list+switch+reveal-admin, the
                            //    conversations-clearing switch, the state-dir scoping switch,
                            //    the require-confirm gate) are all green with this shape.
                            let machine = fauna_launch_machine::LaunchMachine::new(
                                std::sync::Arc::new(fauna_launch_machine::NullObserver),
                                std::sync::Arc::new(launch_persistence()),
                            );
                            let machine_for_start = std::sync::Arc::clone(&machine);
                            crate::async_helper::run_on_tokio(
                                async move { machine_for_start.start().await },
                                move |()| {
                                    // Swap the process holder to the incoming account
                                    // BEFORE the rebuild, not at its `AuthSuccess`
                                    // (which now answers `Reused`). The rebuild builds
                                    // the Account page synchronously, and that page keys
                                    // "the account this window serves" on the holder
                                    // (`fauna_client_accounts::session_account`) — so a
                                    // holder still naming the outgoing account rendered
                                    // THAT row as the served one (non-activatable, the
                                    // active indicator, no remove button) and the
                                    // incoming account's row as a switch target. A
                                    // second switch back then clicked a row that runs
                                    // nothing. The outgoing account's scoped state is
                                    // already closed (`reset_actor_scoped_state` above),
                                    // so this is the swap `account-scoping.md`
                                    // § Concurrent instances prescribes, at the earliest
                                    // point it is safe. Bound-or-refuse is unchanged: a
                                    // bound secondary switching off its binding is
                                    // refused here exactly as `AuthSuccess` refused it.
                                    if let Err(refusal) =
                                        crate::account_scope::become_session_instance(&actor_id)
                                    {
                                        refusal.exit(&actor_id);
                                    }
                                    let result = launch_authenticated(
                                        &app,
                                        &node_url,
                                        &secret_hex,
                                        machine,
                                        |_| {},
                                    );

                                    // 4. Re-point the test-command drain's cells at the new window
                                    //    (e2e nav after a switch); inert for real users (registry
                                    //    empty). Then present. We must NOT start a second drain
                                    //    (start_test_agent_if_enabled) — the first launch's 50ms
                                    //    poll loop is still running.
                                    crate::rebind_active_window(
                                        result.stack.clone(),
                                        result.state.clone(),
                                        result.error_label.clone(),
                                        result.warning_label.clone(),
                                        result.info_label.clone(),
                                        std::rc::Rc::clone(&result.client),
                                        std::rc::Rc::clone(&result.open_profile),
                                    );
                                    show_window(&result.window);
                                    // Rebuild complete — allow the next switch.
                                    pending.set(false);
                                },
                            );
                        },
                    );
                    // Every other surface was built synchronously — allow the next switch.
                    if !rebuilds_async {
                        pending.set(false);
                    }
                },
            );
        });
    });
}

// ---------------------------------------------------------------------------
// Launch the authenticated main window. Returns the MainWindowResult so the
// caller can wire up the test agent and present the window.
// ---------------------------------------------------------------------------

struct LaunchResult {
    window: adw::ApplicationWindow,
    stack: gtk::Stack,
    state: Rc<std::cell::RefCell<app::AppState>>,
    error_label: gtk::Label,
    warning_label: gtk::Label,
    info_label: gtk::Label,
    client: Rc<client::FaunaClient>,
    /// See [`app::MainWindowResult::open_profile`] — carried out to the four
    /// sites that hand a freshly built main window to the test-command drain.
    open_profile: OpenProfileFn,
}

/// Case-1 launch flow: full identity + nest_url present in libsecret.
/// Presents a launch screen (`views::launch`) while running the silent
/// challenge handshake, then routes per the four documented outcomes:
///
/// | Outcome              | Action                                           |
/// |----------------------|--------------------------------------------------|
/// | Authenticated        | Close launch, build & present main window.       |
/// | Unregistered (404)   | Close launch, seed identity, wizard@invite_request (with saved nest_url + cached handle). |
/// | Transient (network)  | Switch launch screen to retry CTA. User can hit Retry to re-run the challenge, or Use-different-nest to fall through. |
/// | Unreachable (other)  | Close launch, seed identity, wizard@handle_entry — let user pick a different domain. |
///
/// The unreadable-index floor's confirm body, with its refusal passed in —
/// the same shape as `settings::account::sign_out_confirm_unless`, so a test can reach the gesture without a live
/// sibling on this machine's real config dirs (the question's own answer is
/// pinned in `account_scope` over temp bases).
///
/// The refusal is read off the still-held view (nothing was taken yet); the
/// window is taken and destroyed only once the erase is allowed to run —
/// `trigger_sign_out`'s handler has no half that is safe under a live
/// sibling.
fn start_over_confirm_unless(
    view_holder: &Rc<std::cell::RefCell<Option<views::launch::LaunchView>>>,
    blocked: impl FnOnce() -> Option<String>,
) {
    if let Some(line) = blocked() {
        if let Some(v) = view_holder.borrow().as_ref() {
            crate::settings::render_error_label(&v.error_label, Some(&line));
        }
        return;
    }
    if let Some(v) = view_holder.borrow_mut().take() {
        v.window.destroy();
    }
    crate::settings::trigger_sign_out();
}

/// Implementation pattern: `LaunchView` lives in
/// `Rc<RefCell<Option<LaunchView>>>` so the retry / fallthrough button
/// callbacks can re-enter the silent-challenge flow or take the view
/// for teardown.
fn launch_silent_challenge_flow(
    application: &adw::Application,
    node_url: String,
    secret_hex: String,
) {
    use std::cell::RefCell;

    let view_holder: Rc<RefCell<Option<views::launch::LaunchView>>> = Rc::new(RefCell::new(None));

    // The machine that classified the CURRENT launch attempt. `trust_nest_identity()`
    // is only meaningful on the machine actually sitting in `IdentityChanged` (it
    // reads the secret + nest_url off that state), so the trust button must drive
    // *that* machine — not a fresh one, which would re-challenge from Boot and
    // never reach the forget-the-pin branch. Populated by `handle_launch_phase`
    // (which already receives the `Arc`), read by the `on_trust` callback below.
    let machine_holder: Rc<RefCell<Option<std::sync::Arc<fauna_launch_machine::LaunchMachine>>>> =
        Rc::new(RefCell::new(None));

    let view = views::launch::build_launch_window(
        application,
        // Retry: switch back to Launching phase, re-run the challenge.
        {
            let app = application.clone();
            let node_url = node_url.clone();
            let secret_hex = secret_hex.clone();
            let view_holder = view_holder.clone();
            let machine_holder = machine_holder.clone();
            move || {
                if let Some(v) = view_holder.borrow().as_ref() {
                    (v.set_phase)(views::launch::LaunchPhase::Launching);
                }
                run_silent_challenge_async(
                    app.clone(),
                    node_url.clone(),
                    secret_hex.clone(),
                    view_holder.clone(),
                    machine_holder.clone(),
                );
            }
        },
        // Fallthrough: close launch, seed identity, open wizard at handle_entry.
        {
            let app = application.clone();
            let secret_hex = secret_hex.clone();
            let view_holder = view_holder.clone();
            move || {
                if let Some(v) = view_holder.borrow_mut().take() {
                    // destroy() bypasses connect_close_request — close()
                    // would fire the user-X-clicks handler that calls
                    // app.quit().
                    v.window.destroy();
                }
                // Walk away from this nest, keeping the identity — the
                // registry-routed shape apple's `useADifferentNest` ships
                // (account-scoping.md § Concurrent instances, the delete
                // corollary). Without this the stale (nest_url, device_id)
                // binding survives an abandoned re-onboard: relaunch would
                // silently retry the same unreachable nest instead of
                // landing back on this fallthrough. Best-effort: the wizard
                // seed below carries the identity regardless.
                if let Some(actor) = account_scope::active_actor_id_hex() {
                    let _ = account_registry().clear_nest_binding(&actor);
                }
                let onboarding =
                    views::onboarding::build_onboarding_window_with_seed(&app, &secret_hex);
                start_test_agent_if_enabled(
                    &app,
                    None,
                    None,
                    Some(onboarding.error_label.clone()),
                    None,
                    None,
                    None,
                    Some(onboarding.machine.clone()),
                    None,
                );
                show_window(&onboarding.window);
            }
        },
        // Recover: close launch, seed identity for recovery, open the wizard on
        // nest_recovery with the launch-time box list pushed in (box-recovery.md
        // § Recovery UI (step 4), the surviving-device entry). Mirrors
        // fallthrough but drives the recovery branch, matching the web
        // `recoverFromLaunch` (seedIdentityForRecovery + setRecoveryBoxes).
        {
            let app = application.clone();
            let secret_hex = secret_hex.clone();
            let view_holder = view_holder.clone();
            move |boxes: Vec<String>| {
                if let Some(v) = view_holder.borrow_mut().take() {
                    v.window.destroy();
                }
                let onboarding =
                    views::onboarding::build_onboarding_window_with_seed(&app, &secret_hex);
                // seed_identity_for_recovery is a superset of the build-time
                // seed_identity: same imported_secret, but flips recovery_intent
                // + recovery_came_from=Launch and lands the step on NestRecovery
                // (the observer then swaps the stack to that page). Push the box
                // list so nest_recovery renders `recover-box-item` rows.
                onboarding
                    .machine
                    .seed_identity_for_recovery(secret_hex.clone());
                onboarding.machine.set_recovery_boxes(boxes);
                start_test_agent_if_enabled(
                    &app,
                    None,
                    None,
                    Some(onboarding.error_label.clone()),
                    None,
                    None,
                    None,
                    Some(onboarding.machine.clone()),
                    None,
                );
                show_window(&onboarding.window);
            }
        },
        // Trust this nest (`nest-identity-changed-trust-button`): forget the TOFU
        // pin and re-TOFU — `LaunchMachine::trust_nest_identity()` does both, then
        // re-runs the silent challenge, so the result lands back in
        // `handle_launch_phase` exactly like a first launch. This is the ONLY path
        // that forgets a pin (security.md § Transport trust: never a
        // silent re-pin), and it is a no-op on any machine not in `IdentityChanged`.
        {
            let app = application.clone();
            let node_url = node_url.clone();
            let secret_hex = secret_hex.clone();
            let view_holder = view_holder.clone();
            let machine_holder = machine_holder.clone();
            move || {
                let Some(machine) = machine_holder.borrow().clone() else {
                    tracing::error!("[launch] trust clicked with no machine in flight");
                    return;
                };
                if let Some(v) = view_holder.borrow().as_ref() {
                    (v.set_phase)(views::launch::LaunchPhase::Launching);
                }
                let app = app.clone();
                let node_url = node_url.clone();
                let secret_hex = secret_hex.clone();
                let view_holder = view_holder.clone();
                let machine_holder = machine_holder.clone();
                crate::async_helper::run_on_tokio(
                    async move {
                        machine.trust_nest_identity().await;
                        (machine.snapshot(), machine)
                    },
                    move |(snapshot, machine)| {
                        *machine_holder.borrow_mut() = Some(std::sync::Arc::clone(&machine));
                        handle_launch_phase(
                            app,
                            node_url,
                            secret_hex,
                            view_holder,
                            snapshot,
                            machine,
                        );
                    },
                );
            }
        },
        // `account-index-reset-confirm-button`: the malformed verdict's
        // documented floor (`long-term-store.md` § Cleanup contract).
        // `trigger_sign_out` is the same erase-every-account +
        // wipe-credential-namespace + open-onboarding sequence "Sign Out"
        // runs, and its own doc states it is safe to call with no main
        // window present — exactly this pre-auth launch-screen context.
        // `views::launch` guards the click against any other phase, so this
        // is only ever reached from the confirm the malformed verdict shows.
        //
        // MUST destroy() this launch window FIRST: `trigger_sign_out`'s
        // handler tears down every window with `.close()` (correct for the
        // authenticated main window, whose close handler does not quit), but
        // THIS window's `connect_close_request` calls `app.quit()` (the
        // pre-auth-only trap documented above) — routing straight through
        // `.close()` quit the whole process mid-teardown, right after the
        // onboarding window it built (measured: `handle_change` for
        // `IdentityChoice` ran, then the process exited). `destroy()`
        // bypasses that handler, exactly like every other exit above.
        //
        // ⚠ Refused — BEFORE the window goes — while another instance serves
        // an account the erase would reach (`account-scoping.md` § Concurrent
        // instances → *An erase refuses while a sibling serves the account*):
        // it is the sign-out's erase, so it asks the sign-out's question. The
        // e2e agent's `reset|logout` exemption is for a reset with no user in
        // front of it; this is a user's confirm button. The line lands on the
        // launch screen's `error-message` with the confirm still showing, so
        // closing the other window and confirming again is the whole remedy.
        {
            let view_holder = view_holder.clone();
            move || {
                start_over_confirm_unless(&view_holder, crate::account_scope::start_over_blocked);
            }
        },
    );

    if let Some(launch_error_label) = Some(view.error_label.clone()) {
        // Mirror the launch screen's error label into the test agent so
        // E2E can read its content, same convention as the onboarding
        // wizard's error label. Test agent for the launch screen has
        // no machine handle (the wizard isn't started yet) and no
        // FaunaClient (silent challenge doesn't construct one until
        // success).
        start_test_agent_if_enabled(
            application,
            None,
            None,
            Some(launch_error_label),
            None,
            None,
            None,
            None,
            None,
        );
    }

    show_window(&view.window);
    *view_holder.borrow_mut() = Some(view);

    // Kick off the first silent challenge.
    run_silent_challenge_async(
        application.clone(),
        node_url,
        secret_hex,
        view_holder,
        machine_holder,
    );
}

/// Spawn a tokio worker to run the silent challenge via the shared
/// `LaunchMachine`, then route the resulting snapshot on the GTK main
/// loop via `handle_launch_phase`.
///
/// LaunchMachine is constructed fresh per call — no state survives
/// between attempts. That's intentional: the user's "Retry" click on
/// `LaunchView` re-invokes this function, which creates a new machine
/// and re-runs `start()`. On the `Online` path the same `LaunchMachine`
/// is handed to `FaunaClient`, whose `NestContentApi` reads the bearer
/// via `current_bearer()` (with the TTL pre-expiry buffer) and calls
/// `notify_401()` on a stale-bearer 401 — see `crate::client` and
/// `crate::nest_content_api`. (`launch_authenticated` re-runs `refresh_token()`
/// once via `FaunaClient::authenticate` to surface the `AuthSuccess` UI signal;
/// that's a redundant probe we can drop once the launch flow passes the
/// silent-challenge bearer through directly.)
/// The `LaunchPersistence` adapter `LaunchMachine::start()` branches on, over
/// the live libsecret-backed account registry. Cheap to build (a stateless view
/// over the store), so callers construct one per read rather than threading it.
/// The launch adapter every launch-time read goes through — **binding-aware**,
/// so a secondary instance never reads or writes the active account's slots.
///
/// One seam, not a branch per caller: `has_awaiting_dns_slot`, the case-2
/// pending-invite read and the `LaunchMachine` in `run_silent_challenge_async`
/// all resolve through here, so binding this process (the chooser's pick, or a
/// `FAUNA_BOUND_ACCOUNT` launch) redirects the whole launch at once. A bound
/// adapter resolves the named account's slots and never consults or moves
/// `active` (`account-scoping.md` § Concurrent instances → "The active
/// pointer decouples from the session").
fn launch_persistence() -> fauna_client_accounts::RegistryLaunchPersistence {
    let registry = account_registry();
    match session_binding(&registry) {
        Some(actor) => registry.bound_launch_persistence(actor),
        None => registry.launch_persistence(),
    }
}

/// This process's launch binding **as the account it names today**: a bound
/// launch whose named id has a recorded successor in this install's registry
/// binds to the terminal successor (`account-scoping.md` § Concurrent
/// instances → *The binding follows the account*, rider 2). Every
/// identity-consuming read of the binding — the bound launch persistence and
/// the bound session material — goes through this rather than the bare cell,
/// so a spawn minted before this install learned of a succession (a sibling
/// seat's ceremony, a phrase-only restore) comes up as the successor instead
/// of resolving the retired id and meeting the nest's `superseded` refusal.
/// The resolver re-points the process cell, so the "is bound at all" reads
/// (`fauna_client_accounts::refuse_if_bound_from_onboarding`) need no change.
/// Idempotent and free for a never-succeeded binding.
fn session_binding(registry: &fauna_client_accounts::AccountRegistry) -> Option<String> {
    registry.resolve_launch_binding()
}

/// The live libsecret-backed account registry — linux's **single** registry
/// construction point, so mutation locking is decided here once for every
/// writer in the client (`long-term-store.md` § Multi-account evolution ->
/// Cross-process mutation lock: *"one construction site per client decides
/// locking for all of its writers"*). Cheap: a stateless view over the store
/// plus a path, so callers build one per use rather than threading it around.
///
/// Mutators serialize under an advisory OS file lock in the **install-scoped**
/// state base — [`account_scope::install_state_base`], the same
/// `<xdg-config>/fauna/` the per-account scope dirs and the
/// `AccountInstanceLock` files live in. Install-scoped is required, not
/// incidental: the lock guards the `fauna/index` blob that is shared *between*
/// accounts, so a per-account lock would let two instances rewrite one index
/// while each held its own file. Reads and the `bind_account` spawn gate never
/// acquire it, so launch stays wait-free. apple's twin is
/// `FaunaAccounts.registry()`.
///
/// No resolvable base degrades to the no-op lock — today's unserialized
/// behavior — rather than refusing to build a registry: the lock narrows a
/// race, it must never widen a failure.
///
/// ⚠ Do not mint an `AccountRegistry` anywhere else. A registry built past
/// this function carries the no-op lock, and nothing observable fails — the
/// write succeeds while silently skipping the lock every other writer takes.
/// `account_registry_census_test` fails the build if one appears.
pub(crate) fn account_registry() -> fauna_client_accounts::AccountRegistry {
    account_registry_over(
        std::sync::Arc::new(client::secret_store()),
        account_scope::install_state_base(),
    )
}

/// The account's attested predecessor ids
/// (`AccountRegistry::attested_predecessor_actor_ids`) for the identity a hex
/// secret derives — the reader-side input the shared seats judge a retired
/// identity's row with, in place of a succession lookup (writer-signed change
/// records, ruling (8)(b) source (ii)). Empty for a malformed secret or an
/// identity that never succeeded.
pub(crate) fn attested_predecessor_ids_for_secret_hex(secret_hex: &str) -> Vec<[u8; 32]> {
    let Ok(keypair) = fauna_core::identity::ActorKeypair::from_secret_hex(secret_hex) else {
        return Vec::new();
    };
    account_registry()
        .attested_predecessor_actor_ids(&keypair.actor_id_hex())
        .into_iter()
        .map(|id| id.0)
        .collect()
}

/// The choke point's body, over an explicit store and mutation-lock base —
/// still the one place a registry is built, so a unit test that must reach
/// neither the real keyring nor the process env ([`account_registry_in`]) gets
/// exactly the construction production gets: the erase-complete constructor,
/// and the file lock whenever a base resolves.
fn account_registry_over(
    store: std::sync::Arc<dyn fauna_client_accounts::SecretStore>,
    lock_base: Option<std::path::PathBuf>,
) -> fauna_client_accounts::AccountRegistry {
    match lock_base {
        Some(base) => fauna_credential_store::account_registry_with_lock(
            store,
            std::sync::Arc::new(fauna_client_accounts::FileMutationLock::new(&base)),
        ),
        None => fauna_credential_store::account_registry(store),
    }
}

/// [`account_registry`] over a file-backed store in `dir`, its mutation lock
/// there too — for unit tests (`account_scope`'s remove-account and erase
/// refusals).
#[cfg(test)]
pub(crate) fn account_registry_in(
    dir: std::path::PathBuf,
) -> fauna_client_accounts::AccountRegistry {
    account_registry_over(
        std::sync::Arc::new(fauna_credential_store::CredentialStore::with_file_backend(
            "fauna-linux-test",
            dir.clone(),
        )),
        Some(dir),
    )
}

/// The credentials this launch routes on — **binding-aware**, the read half
/// of the same decoupling [`launch_persistence`] does for the launch adapter.
///
/// A plain launch reads the *active* account — correct, because a plain launch
/// binds to `active`. A **bound** launch must not: reading `active` would build
/// the session as the wrong account while the launch machine routed on the
/// right one (the actor-blind app-root blocker `account-scoping.md`
/// § Implementation status describes, proven on apple before its
/// `sessionMaterial` adoption). Both resolve the shared per-account read
/// `AccountRegistry::session_material` — one account-resolved accessor per
/// process, never per-call-site scoping.
///
/// `None` for a binding whose secret cannot be resolved: fail closed, exactly
/// as `session_material` does. Routing then falls to the wizard cases, which
/// `fauna_client_accounts::refuse_if_bound_from_onboarding` terminates — a
/// bound launch never silently degrades into a plain one.
fn launch_credentials() -> Option<(String, String, String)> {
    let registry = account_registry();
    // Bound to one account, else the registry's active pointer — the same
    // two-way resolution `session::stored_account` makes on tui.
    let actor = session_binding(&registry).or_else(|| registry.active())?;
    registry.session_material(&actor).map(|m| {
        (
            m.nest_url.unwrap_or_default(),
            m.secret_hex.as_str().to_string(),
            m.device_id.unwrap_or_default(),
        )
    })
}

/// Is a deferred-DNS nest waiting in the account registry?
///
/// Read through `RegistryLaunchPersistence` — the same `LaunchPersistence`
/// adapter `LaunchMachine::start()` branches on — so the gate and the machine
/// can never disagree about whether the row applies.
fn has_awaiting_dns_slot() -> bool {
    use fauna_launch_machine::LaunchPersistence;
    launch_persistence().load_awaiting_dns().is_some()
}

fn run_silent_challenge_async(
    app: adw::Application,
    node_url: String,
    secret_hex: String,
    view_holder: Rc<std::cell::RefCell<Option<views::launch::LaunchView>>>,
    // Stashed so the `nest-identity-changed-trust-button` can call
    // `trust_nest_identity()` on the machine that actually produced the
    // `IdentityChanged` verdict (it re-reads secret + nest_url off that state).
    machine_holder: Rc<
        std::cell::RefCell<Option<std::sync::Arc<fauna_launch_machine::LaunchMachine>>>,
    >,
) {
    use std::sync::Arc;

    let machine = fauna_launch_machine::LaunchMachine::new(
        Arc::new(fauna_launch_machine::NullObserver),
        // Route the launch machine through the multi-account registry: it reads
        // the ACTIVE account's slots; switching accounts is `set_active` + a
        // teardown/rebuild of this machine (Stage 1 switcher work).
        // See docs/goal/architecture/long-term-store.md § Shared seam.
        Arc::new(launch_persistence()),
    );
    // Clone for the GTK-thread callback so handle_launch_phase can pass
    // the same machine into FaunaClient (FaunaClient uses it for token
    // refresh during the session via current_bearer / notify_401).
    let machine_for_callback = Arc::clone(&machine);

    crate::async_helper::run_on_tokio(
        async move {
            machine.start().await;
            machine.snapshot()
        },
        move |snapshot| {
            *machine_holder.borrow_mut() = Some(Arc::clone(&machine_for_callback));
            handle_launch_phase(
                app,
                node_url,
                secret_hex,
                view_holder,
                snapshot,
                machine_for_callback,
            );
        },
    );
}

/// Dispatch the LaunchMachine snapshot to the appropriate next view.
/// Phase mapping per `docs/goal/behavior/onboarding.md` § App-launch routing:
///
/// - `Online` → main app via `launch_authenticated`, which runs a background
///   silent-sign-in to refresh the server-data display cache (the registry
///   launch adapter persists the cache into the account index, not the linux
///   `account=handle/domain/tier` display slots — so the launch refreshes them).
/// - `WizardAt(InviteRequest)` → wizard with `seed_identity` +
///   `navigate_to_invite_request_for_known_nest(nest_url, cached_handle)`.
///   Reached when /verify returns 404 against a claimed nest (or when
///   setup-status didn't answer cleanly — safer-default fallback).
/// - `WizardAt(ClaimCode)` → wizard with `seed_identity` +
///   `navigate_to_claim_code_for_known_nest(nest_url, cached_handle)`.
///   Reached when /verify returns 404 AND `setup-status.claimed == false`
///   — the nest is up but unclaimed, so the user must claim it.
/// - `Offline { transient: true }` → keep the launch window, show the
///   retry CTA (Retry / "Use a different nest") — a reachability failure
///   the client can't reliably classify, so always offer Retry.
/// - `Offline { transient: false }` → terminal. Today the only such case the
///   launch flow produces is the nest authoritatively reporting it is outdated
///   (`fauna.nest.outdated` → degraded mode), which is NON-retryable → route to
///   the NeedsUpdate surface (localized message, no Retry button;
///   version-compatibility.md Dim 4 / onboarding.md § App-launch routing).
/// - `WizardAt(HandleEntry | IdentityChoice)` shouldn't appear in this
///   flow (we only run silent challenge with a `nest_url` present);
///   if the persistence read changed mid-flight we surface as
///   transient with a generic message.
fn handle_launch_phase(
    app: adw::Application,
    node_url: String,
    secret_hex: String,
    view_holder: Rc<std::cell::RefCell<Option<views::launch::LaunchView>>>,
    snapshot: fauna_launch_machine::LaunchSnapshot,
    machine: std::sync::Arc<fauna_launch_machine::LaunchMachine>,
) {
    use fauna_launch_machine::{LaunchPersistence, LaunchPhase, LaunchWizardEntry};

    match snapshot.phase {
        LaunchPhase::Online => {
            // The registry adapter's `save_authenticated` persisted nest_url +
            // the server-data cache into the account index, but (unlike the old
            // `LibsecretLaunchPersistence`) it does NOT write the linux display
            // cache (`account=handle/domain/tier` + the in-process mirror). So
            // run the in-launch silent-sign-in to refresh that display cache —
            // one extra round-trip on launch, the pre-optimization behaviour.
            // (Unifying the display cache onto the registry index is a Stage-1
            // switcher follow-up.)
            if let Some(v) = view_holder.borrow_mut().take() {
                // destroy() bypasses connect_close_request — close() would
                // fire the user-X-clicks handler that calls app.quit().
                v.window.destroy();
            }
            let result = launch_authenticated(&app, &node_url, &secret_hex, machine, |_| {});
            start_test_agent_if_enabled(
                &app,
                Some(result.stack),
                Some(result.state),
                Some(result.error_label),
                Some(result.warning_label),
                Some(result.info_label),
                Some(result.client),
                None,
                Some(result.open_profile),
            );
            // The only site an auto-started launch reaches: routing landed
            // Online, so there is a session to be resident *for*. Every other
            // `show_window` stays unconditional — the onboarding routes below
            // must show even under `--autostart` (a signed-out auto-start has
            // to be loud, not a silently dead agent), and the account-switch /
            // relaunch sites happen in a session the user is already driving.
            show_main_window_unless_autostart_hidden(&result.window);
        }
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::AwaitingManualDns,
        } => {
            // The deferred-DNS row. The machine checks it BEFORE the
            // silent-challenge row, because while the records are not yet at the
            // registrar the nest is unreachable *by definition* — a challenge
            // could only fail through to the retry surface (onboarding.md
            // § App-launch routing).
            //
            // Note what this arm does NOT use: `node_url`. The deferred-DNS exit
            // deliberately writes no nest_url on the identity (the nest is not
            // claimed yet), so the store has none. The record carries its own,
            // and we re-read it through the SAME `LaunchPersistence` the machine
            // branched on — so the record we seed is byte-for-byte the record it
            // routed on, with no second source of truth (onboarding.md
            // § Long-term store contract).
            if let Some(v) = view_holder.borrow_mut().take() {
                // destroy() bypasses connect_close_request — close() would
                // fire the user-X-clicks handler that calls app.quit().
                v.window.destroy();
            }
            let persistence = launch_persistence();
            let Some(rec) = persistence.load_awaiting_dns() else {
                // The machine routed here off this very slot, so its absence now
                // means the store changed underneath us. Falling through to the
                // ordinary wizard is the safe read: the identity survives, and
                // the user re-enters at handle_entry rather than staring at an
                // empty "Almost ready" page with no records to add.
                tracing::error!(
                    "[launch] AwaitingManualDns row with no awaiting-DNS slot; \
                     falling back to the wizard"
                );
                let onboarding =
                    views::onboarding::build_onboarding_window_with_seed(&app, &secret_hex);
                show_window(&onboarding.window);
                return;
            };
            let onboarding =
                views::onboarding::build_onboarding_window_awaiting_dns(&app, &secret_hex, rec);
            start_test_agent_if_enabled(
                &app,
                None,
                None,
                Some(onboarding.error_label.clone()),
                None,
                None,
                None,
                Some(onboarding.machine.clone()),
                None,
            );
            show_window(&onboarding.window);
        }
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::InviteRequest,
        } => {
            // /auth/verify returned 404 — actor not registered on this
            // nest. Seed identity + nest_url and land the wizard at
            // invite_request so the user can submit/restore an invite
            // without retyping their handle.
            if let Some(v) = view_holder.borrow_mut().take() {
                // destroy() bypasses connect_close_request — close() would
                // fire the user-X-clicks handler that calls app.quit().
                v.window.destroy();
            }
            let onboarding =
                views::onboarding::build_onboarding_window_with_seed(&app, &secret_hex);
            let cached_handle = client::load_account_cache().0.unwrap_or_default();
            onboarding
                .machine
                .navigate_to_invite_request_for_known_nest(node_url, cached_handle);
            start_test_agent_if_enabled(
                &app,
                None,
                None,
                Some(onboarding.error_label.clone()),
                None,
                None,
                None,
                Some(onboarding.machine.clone()),
                None,
            );
            show_window(&onboarding.window);
        }
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::ClaimCode,
        } => {
            // /auth/verify returned 404 AND setup-status reports
            // claimed=false — the saved nest is up but unclaimed, so
            // the user must claim it themselves rather than ask for an
            // invite. Seed identity + nest_url and land the wizard at
            // claim_code. See `docs/goal/behavior/onboarding.md` § App-launch
            // routing — silent-challenge fallback table (unclaimed-nest
            // row).
            if let Some(v) = view_holder.borrow_mut().take() {
                // destroy() bypasses connect_close_request — close() would
                // fire the user-X-clicks handler that calls app.quit().
                v.window.destroy();
            }
            let onboarding =
                views::onboarding::build_onboarding_window_with_seed(&app, &secret_hex);
            let cached_handle = client::load_account_cache().0.unwrap_or_default();
            onboarding
                .machine
                .navigate_to_claim_code_for_known_nest(node_url, cached_handle);
            start_test_agent_if_enabled(
                &app,
                None,
                None,
                Some(onboarding.error_label.clone()),
                None,
                None,
                None,
                Some(onboarding.machine.clone()),
                None,
            );
            show_window(&onboarding.window);
        }
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::PendingFactoryReset,
        } => {
            // Factory-reset resume (gap CR-1, `common.md` § Client-state
            // recoverability). The admin dispatched a factory reset and this
            // client died before the re-claim completed — possibly before the
            // reply that carried the claim code ever rendered. The code survives
            // only because it was minted and persisted before dispatch, so seed
            // the wizard's claim page from the slot: same surface as `ClaimCode`
            // above, but pre-filled. Read back through the same adapter the
            // launch machine branched on, so the record we seed is the record it
            // routed on.
            if let Some(v) = view_holder.borrow_mut().take() {
                v.window.destroy();
            }
            let onboarding =
                views::onboarding::build_onboarding_window_with_seed(&app, &secret_hex);
            if let Some(rec) = launch_persistence().load_pending_factory_reset() {
                onboarding
                    .machine
                    .navigate_to_claim_code_for_known_nest_with_code(
                        rec.nest_url,
                        rec.handle,
                        rec.claim_code,
                    );
            }
            start_test_agent_if_enabled(
                &app,
                None,
                None,
                Some(onboarding.error_label.clone()),
                None,
                None,
                None,
                Some(onboarding.machine.clone()),
                None,
            );
            show_window(&onboarding.window);
        }
        // The saved account index is present and this build cannot use it
        // (`version-compatibility.md` § 5 item 9). Checked BEFORE every other
        // row, `superseded_successor` included — the machine reads this off
        // `LaunchPersistence::account_index_refusal` before it even attempts
        // `load_identity`, and it rides the same additive-side-channel
        // pattern (`LaunchSnapshot::account_index_refusal`), so an app that
        // cannot yet render it still stops at `Offline { transient: false }`
        // and shows `last_error`. Twin of `apps/fauna-tui/src/launch.rs`'s
        // `route()` guard.
        LaunchPhase::Offline { .. } if snapshot.account_index_refusal.is_some() => {
            let refusal = snapshot.account_index_refusal.expect("guarded by is_some");
            tracing::error!("[launch] the saved account index is unreadable: {refusal:?}");
            if let Some(v) = view_holder.borrow().as_ref() {
                (v.set_phase)(views::launch::LaunchPhase::AccountIndexUnreadable {
                    refusal,
                    confirming: false,
                });
            }
        }
        // The identity was succeeded — this account answers to someone else's
        // keypair now (`identity-succession.md` § Propagation → *Own device
        // fleet*: "the client surfaces 'this identity was succeeded — import the
        // new identity'"). The twin of `apps/fauna-tui/src/launch.rs`'s arm.
        //
        // Checked BEFORE the generic `Offline` arms below, because the machine
        // deliberately projects this to `Offline { transient: false }`: the
        // successor rides the snapshot side channel, not the phase, so apps that
        // cannot yet render it still stop retrying. Without this arm linux earns
        // the refusal and then shows the NeedsUpdate surface — a version-mismatch
        // message for an identity problem, with no way out.
        //
        // **No new ui.yaml elements.** The affordance IS the existing import flow
        // (page `identity_import`), with the explanation on that page's existing
        // `error-message` — the same settlement tui made.
        //
        // **The message deliberately does not name the successor.** The refusal's
        // successor is *claimed* until `fauna_client_recovery::resolve_successor`
        // has verified it against the registration chain; presenting it as fact
        // would make this client trust the nest as an authorizer. It goes to the
        // log, where a fleet admin needs it, and not to the screen.
        LaunchPhase::Offline { .. } if snapshot.superseded_successor.is_some() => {
            let claimed = snapshot.superseded_successor.clone().unwrap_or_default();
            tracing::error!(
                "[launch-machine] this identity was succeeded (claimed successor {claimed}) — \
                 routing to the identity-import flow"
            );
            if let Some(v) = view_holder.borrow_mut().take() {
                // destroy() bypasses connect_close_request — close() would
                // fire the user-X-clicks handler that calls app.quit().
                v.window.destroy();
            }
            let onboarding =
                views::onboarding::build_onboarding_window_with_seed(&app, &secret_hex);
            // The reason goes through the machine, not the label: this view
            // mirrors `error_message()` into `error_label` on every observer
            // tick, so a direct `set_text` would be erased by the tick this very
            // transition fires (`views::onboarding::handle_change`).
            onboarding.machine.begin_import_identity_with_reason(
                crate::i18n::strings::onboarding::launch::IDENTITY_SUPERSEDED.to_string(),
            );
            // Best-effort: prove the claim against the registration chain, then
            // upgrade the message to name the successor. The screen is already
            // painted from the refusal alone — this only ever makes it say more,
            // and every failure leaves the claim-free message standing.
            //
            // `run_on_tokio` lands the callback back on the GTK main thread, so
            // it can drive the machine directly; no message plumbing needed.
            {
                let machine_for_naming = onboarding.machine.clone();
                let nest_url = node_url.clone();
                let secret = secret_hex.clone();
                let secret_for_adoption = secret_hex.clone();
                crate::async_helper::run_on_tokio(
                    async move { client::verify_succession_successor(&nest_url, &secret).await },
                    move |successor| {
                        let Some(successor) = successor else { return };
                        // Guarded on still being on the screen this upgrades: the
                        // verify can land after the user navigated away, and
                        // re-asserting a supersession over whatever they are doing
                        // now would be a banner from a flow they already handled.
                        //
                        // Save in the one case with nothing left to import: this
                        // device already holds the PROVEN successor's key — the
                        // state a lost succession reply leaves behind, whose
                        // message promised that reopening the app signs in as it.
                        // Then the screen is not upgraded but replaced, by the
                        // switch (`settings::adopt_held_successor`).
                        if machine_for_naming.step()
                            == fauna_onboarding_machine::OnboardingStep::IdentityImport
                            && !client::actor_id_from_secret_hex(&secret_for_adoption).is_some_and(
                                |predecessor| {
                                    crate::settings::adopt_held_successor(&predecessor, &successor)
                                },
                            )
                        {
                            machine_for_naming.begin_import_identity_with_reason(
                                crate::i18n::strings::onboarding::launch::IDENTITY_SUPERSEDED_VERIFIED
                                    .replace("{successor}", &successor),
                            );
                        }
                    },
                );
            }
            start_test_agent_if_enabled(
                &app,
                None,
                None,
                Some(onboarding.error_label.clone()),
                None,
                None,
                None,
                Some(onboarding.machine.clone()),
                None,
            );
            show_window(&onboarding.window);
        }
        // A nest this app signed in to before now refuses the identity
        // (`onboarding.md` § App-launch routing — the previously-signed-in
        // row). The machine projects it to `Offline { transient: false }` and
        // carries the verdict on the `sign_in_refused` side channel; checked
        // before the generic arm below, which would paint it as NeedsUpdate.
        // Twin of `apps/fauna-tui/src/launch.rs`'s arm.
        LaunchPhase::Offline { .. } if snapshot.sign_in_refused => {
            let error = snapshot.last_error.unwrap_or_default();
            tracing::error!(
                "[launch-machine] the saved nest no longer signs this identity in: {error}"
            );
            if let Some(v) = view_holder.borrow().as_ref() {
                (v.set_phase)(views::launch::LaunchPhase::SignInRefused { error });
            }
        }
        LaunchPhase::Offline { transient } => {
            // `transient: true` → a reachability failure (5xx, timeout, DNS,
            // refused, decommissioned): show the retry CTA (Retry re-runs the
            // silent challenge; "Use a different nest" mounts the wizard at
            // handle_entry). `transient: false` → terminal; today that is the
            // nest reporting it is outdated (`fauna.nest.outdated`), which is
            // NON-retryable, so route to the NeedsUpdate surface — the localized
            // message with no Retry button (version-compatibility.md Dim 4).
            let error = snapshot.last_error.unwrap_or_default();
            tracing::error!(
                "[launch-machine] silent-challenge failure (transient={transient}): {error}"
            );
            if let Some(v) = view_holder.borrow().as_ref() {
                let phase = if transient {
                    views::launch::LaunchPhase::TransientRetry { error }
                } else {
                    views::launch::LaunchPhase::NeedsUpdate { error }
                };
                (v.set_phase)(phase);
            }
            // On a transient failure, best-effort read the custodied box list to
            // gate the surviving-device `launch-recover-button` (box-recovery.md
            // § Recovery UI (step 4)) through the shared pre-login resolver:
            // this device's own account store joined with a cold read from the
            // saved nest (§ The plane-era recovery floor → (b) The reads). The
            // saved nest is often the dead box, so the local read is what reveals
            // the button then. Hex `nest_actor_id`s only (the seed stays
            // Rust-internal), never errors; empty → button hidden (the
            // fresh-client `recover-lost-box-button` is the fallback). NeedsUpdate
            // is skipped: a version-mismatch nest is not a recovery target.
            if transient {
                let node_url = node_url.clone();
                let secret_hex = secret_hex.clone();
                let view_holder = view_holder.clone();
                crate::async_helper::run_on_tokio(
                    async move { client::load_recoverable_boxes(Some(&node_url), &secret_hex).await },
                    move |boxes| {
                        if let Some(v) = view_holder.borrow().as_ref() {
                            (v.set_recover_boxes)(boxes);
                        }
                    },
                );
            }
        }
        LaunchPhase::IdentityChanged { .. } => {
            // The nest's pinned deployment identity changed, or a pinned nest can
            // no longer prove any identity (security.md § Transport trust — the SSH `known_hosts` model). Auto-entry is BLOCKED and the
            // bearer already dropped machine-side; surface the localized warning
            // with NO Retry CTA (a retry cannot change the verdict and must never
            // silently re-pin) and only "use a different nest".
            //
            // Before this arm existed, this phase fell into the `other =>`
            // catch-all below and rendered a *dead* Retry button — dead because
            // `retry_silent_challenge()` no-ops outside `Offline{transient:true}` —
            // over a raw `{other:?}` debug dump. That is the one surface where a
            // retry loop is most harmful, so it gets an explicit arm.
            //
            // The surface carries the full uniform model — warning + the "trust
            // this nest" button (which drives `LaunchMachine::trust_nest_identity()`
            // on the machine held in `machine_holder`) + the fallthrough.
            // `ui.yaml`'s `launch_identity_changed` was widened out of
            // `platforms: [web]` to all apps (user-approved, rule A, 2026-07-13).
            tracing::error!(
                "[launch-machine] nest identity changed: {}",
                snapshot.last_error.as_deref().unwrap_or("(no detail)")
            );
            if let Some(v) = view_holder.borrow().as_ref() {
                (v.set_phase)(views::launch::LaunchPhase::IdentityChanged {
                    // The SAME shared string web renders (`i18n/strings/en.yaml`
                    // → onboarding.launch.identity_changed_warning), not the
                    // machine's developer-facing `last_error` — one warning text
                    // on every app (priority #1/#3).
                    error: crate::i18n::strings::onboarding::launch::IDENTITY_CHANGED_WARNING
                        .to_string(),
                });
            }
        }
        other => {
            // Unexpected phase — Boot/Hydrating/SilentChallenge/Refreshing
            // shouldn't appear after `start()` returns; WizardAt(HandleEntry|IdentityChoice)
            // shouldn't appear when nest_url is set. Surface as a
            // transient retry so the user has a recovery path.
            //
            // Empty, not a hand-rolled English sentence: `transient_error_text`
            // falls back to the localized generic retry copy on an empty
            // `error` — this Debug dump is a developer diagnostic, not
            // something translatable, so it stays in the log line below, never
            // on screen.
            tracing::error!("[launch-machine] unexpected phase after start: {other:?}");
            if let Some(v) = view_holder.borrow().as_ref() {
                (v.set_phase)(views::launch::LaunchPhase::TransientRetry {
                    error: String::new(),
                });
            }
        }
    }
}

/// Build the authenticated main window over a fresh `FaunaClient` — the ONE
/// funnel every authenticated launch goes through: a returning user's boot, an
/// account switch, a test-agent session patch, and the post-wizard first sign-in
/// (`views::onboarding::finish_launch_after_signin`). It runs the post-auth hooks
/// every session needs, among them the background silent sign-in that fills the
/// server-data display cache (handle/domain/tier) and pushes `IdentityRefreshed`
/// — the conversations session's only source of its `<handle>@<domain>` send
/// address. (Before the multi-account rewire a `skip_silent` variant existed for
/// the boot path, whose persistence adapter had already written that cache as a
/// side effect; the registry adapter does not, so every launch refreshes it here.)
///
/// `first_setup` runs on the authenticated client just before the window is
/// built: the wizard's first-setup glue (DNS credential, deployment seed, trust
/// mint, mail/DAV provisioning) rides it, and every other launch passes a no-op.
/// ⚠ The post-wizard transition once built its own client, window and pump, and
/// so skipped the silent sign-in — an account signed in through the wizard
/// refused every send with `no_handle` until the app was relaunched.
fn launch_authenticated(
    application: &adw::Application,
    node_url: &str,
    secret_hex: &str,
    machine: std::sync::Arc<fauna_launch_machine::LaunchMachine>,
    first_setup: impl FnOnce(&Rc<client::FaunaClient>),
) -> LaunchResult {
    // The universal post-auth hook is the one place every authenticated launch
    // funnels through (first login, returning-user boot, account switch), so
    // registering here means the *first* successful login wires every later
    // desktop sign-in — which is what keeps the in-process sync engine running
    // for a user who never re-opens the window. Gated + tri-state inside;
    // `apps/linux.md` § Auto-start at sign-in.
    autostart::register_at_post_auth();

    let (tx, rx) = client::ui_channel();
    // The stored `node_url` is the identity truth (it is what the status row
    // below renders and what the registry holds); the socket resolves through
    // the shared dial seam (`fauna_launch_machine::dial`). The two are the same
    // string in every production build; they differ only under the e2e
    // `provider_base_urls["nest"]` override, which is what lets a domain-shaped
    // claim reach a local nest and so drives `onboarding.md` § 3b's derived-ON
    // branch on the post-wizard launch.
    let fauna_client = Rc::new(client::FaunaClient::new(
        fauna_launch_machine::resolved_dial_url(node_url),
        secret_hex.to_string(),
        tx,
        machine,
    ));
    settings::set_client(&fauna_client);
    fauna_client.authenticate();
    // Refresh the server-data cache (handle/domain/tier) from the silent
    // challenge in the background. Best-effort: failures log but don't surface
    // to the UI — cached values stay visible until the refresh completes (or
    // the user re-onboards).
    fauna_client.silent_sign_in();

    // The launch's own glue that needs the authenticated client but must land
    // before the window exists (the wizard's first-setup provisioning).
    first_setup(&fauna_client);

    let result = app::build_main_window(application, &fauna_client);
    result.widgets.status_node_url_row.set_subtitle(node_url);

    // Pre-populate the status bar from the libsecret cache so the user
    // sees their handle immediately on relaunch — without this the row
    // shows blank until /api/v1/account returns. The main flow's
    // `AccountLoaded` handler in `app.rs` will overwrite this with the
    // freshly-fetched value (and re-cache it) once auth completes.
    let (cached_handle, _cached_domain, _cached_tier) = client::load_account_cache();
    if let Some(h) = cached_handle.as_deref()
        && !h.is_empty()
    {
        result.state.borrow_mut().handle = h.to_string();
        result.widgets.status_handle_row.set_subtitle(h);
        settings::set_handle(Some(h.to_string()));
    }

    // Focus tracking for notification suppression.
    result.window.connect_is_active_notify(|win| {
        tray::WINDOW_FOCUSED.store(win.is_active(), Ordering::SeqCst);
    });

    // Close-to-tray handler.
    let stack_for_close = result.stack.clone();
    let cal_state_for_close = Rc::clone(&result.widgets.events_handles.calendar.state);
    result.window.connect_close_request(move |window| {
        let sidebar_item = stack_for_close
            .visible_child_name()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "conversations".into());
        let calendar_view_mode = cal_state_for_close.borrow().view_mode.as_wire().to_string();
        window_state::save_window_state(&window_state::WindowState {
            width: window.width(),
            height: window.height(),
            sidebar_item,
            calendar_view_mode,
        });
        // Sign-out forces destruction so widget callbacks drop their
        // strong refs to the FaunaClient. The flag is one-shot to preserve
        // the user's saved CLOSE_TO_TRAY preference for normal closes.
        if tray::SIGNING_OUT.swap(false, Ordering::SeqCst) {
            // Bounded: sign-out tears the client runtime down right after
            // this handler; a detached flush would race it.
            crate::feed::host::flush_cues_on_close(true);
            crate::conversations::drafts::flush_now_blocking(true);
            crate::feed::drafts::flush_now_blocking(true);
            crate::views::events::drafts::flush_now_blocking(true);
            return glib::Propagation::Proceed;
        }
        if tray::should_hide_to_tray() {
            // Fire-and-forget: hiding to the tray keeps the process (and the
            // client runtime) alive, so the flush completes on its own.
            crate::feed::host::flush_cues_on_close(false);
            window.set_visible(false);
            glib::Propagation::Stop
        } else {
            // Either close-to-tray is off, or it's on but there is no tray host
            // to restore the window from (stock GNOME with no
            // StatusNotifierWatcher) — hiding would strand the window with no
            // affordance to bring it back, so quit instead. The leaked
            // `app.hold()` from `connect_startup` keeps the GApplication alive
            // even with no windows, so without an explicit quit the process
            // would linger headless. Quit so closing the window actually exits,
            // matching Ctrl+Q and the tray "Quit" item.
            //
            // Bounded: `app.quit()` returns to a main loop that exits
            // immediately, so a detached flush would be killed mid-put.
            crate::feed::host::flush_cues_on_close(true);
            crate::conversations::drafts::flush_now_blocking(true);
            crate::feed::drafts::flush_now_blocking(true);
            crate::views::events::drafts::flush_now_blocking(true);
            if let Some(app) = window.application() {
                app.quit();
            }
            glib::Propagation::Proceed
        }
    });

    // UI message pump.
    let state = result.state.clone();
    let widgets = result.widgets;
    // Clone the labels before `widgets` is moved into the closure.
    let error_label = widgets.error_label.clone();
    let warning_label = widgets.warning_label.clone();
    let info_label = widgets.info_label.clone();
    let client_for_msg = Rc::clone(&fauna_client);
    let window_for_poll = result.window.clone();
    let client_for_compose = Rc::clone(&fauna_client);

    // Sign-out flips this flag; the pump then breaks and drops its FaunaClient ref.
    let pump_shutdown = Rc::new(std::cell::Cell::new(false));
    settings::register_pump_shutdown(pump_shutdown.clone());

    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        if pump_shutdown.get() {
            return glib::ControlFlow::Break;
        }
        main_loop_meter::drain("ui-pump", |tick| {
            while let Ok(msg) = rx.try_recv() {
                tick.item(
                    || main_loop_meter::variant_path(&msg),
                    || app::handle_ui_message(&msg, &state, &widgets, &client_for_msg),
                );
            }
        });
        // TRAY_RAISE is consumed by the global poller registered in
        // `connect_startup` (via `app.activate()`), which re-enters
        // `build_ui` and presents the existing main window. We only react
        // to TRAY_COMPOSE here because the compose dialog requires an
        // authenticated FaunaClient.
        if tray::TRAY_COMPOSE.swap(false, Ordering::SeqCst) {
            show_window(&window_for_poll);
            // The previous modal compose dialog disappeared with the
            // unified conversations page. Tray "Compose" now opens the
            // in-pane new-thread compose by routing through the
            // ConversationsManager singleton.
            let _ = &client_for_compose; // keep parameter live for future
            crate::conversations::manager().start_new_conversation();
        }
        glib::ControlFlow::Continue
    });

    // Update check runs on the FaunaClient's tokio runtime via
    // `check_for_updates()` once authentication succeeds (see app.rs).
    // A duplicate `glib::spawn_local` check used to run here, but glib's
    // executor isn't a tokio runtime, so reqwest panicked on DNS resolution.

    LaunchResult {
        window: result.window,
        stack: result.stack,
        state: result.state,
        error_label,
        warning_label,
        info_label,
        client: fauna_client,
        open_profile: result.open_profile,
    }
}

// ---------------------------------------------------------------------------
// Test agent integration — single setup point for both states.
//
// The agent is generic (polls + pushes JSON). This function provides the
// two app-specific pieces: a state serializer and a command handler.
//
// When `stack` is None, the app is on the onboarding screen.
// When `stack` is Some, the app is showing the main window.
// ---------------------------------------------------------------------------

/// Start the in-process element-automation server once (idempotent across the
/// many `start_test_agent_if_enabled` call sites). Gated by
/// `FAUNA_E2E_AGENT_PORT`. The server thread forwards each `/element/*` request
/// here over an async channel; this GTK-main-thread task drains it, runs the op
/// against the live widget tree, and replies. Ref-free — it walks the toplevels
/// fresh per request, so it needs no stage wiring.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn start_element_agent_once(application: &adw::Application) {
    use std::sync::OnceLock;
    static STARTED: OnceLock<()> = OnceLock::new();

    let Some(port) = std::env::var("FAUNA_E2E_AGENT_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
    else {
        return;
    };
    // Register the app so element searches target its active window (excludes
    // stale windows/views from in-process resets); refresh each call is cheap.
    automation::find::set_app(application.upcast_ref());
    if STARTED.set(()).is_err() {
        return; // already started on an earlier call
    }

    let (op_tx, op_rx) = async_channel::unbounded::<automation::agent::ElementOp>();

    // GTK main-thread drain: perform each op against live widgets, reply.
    glib::MainContext::default().spawn_local(async move {
        while let Ok(op) = op_rx.recv().await {
            let result = main_loop_meter::dispatch(
                "agent-element-op",
                || op.req.id.clone(),
                || automation::agent::perform(&op.req),
            );
            let _ = op.reply.send(result);
        }
    });

    // This loop's own liveness stamp, on the same thread as the drain above. An
    // agent timeout alone cannot tell a *stalled* main loop from an op that is
    // merely slow — the drivers issue one op at a time, so the agent never sees
    // the thread serve anything else while it waits. This tick does: if it has
    // not run, the loop is not running. Two relaxed stores a second, e2e-only
    // (this whole function returns early without an agent port), so it costs a
    // real user nothing. `fauna_e2e_agent::UiThreadHeartbeat` § reading a
    // timeout's verdict.
    let heartbeat = fauna_e2e_agent::UiThreadHeartbeat::new();
    let beating = heartbeat.clone();
    glib::timeout_add_local(fauna_e2e_agent::HEARTBEAT_CADENCE, move || {
        beating.beat();
        glib::ControlFlow::Continue
    });

    // The heartbeat's other half: it says the thread is running, this says
    // what the running thread is spending itself on (`main_loop_meter`), so a
    // starvation with the heartbeat beating names its saturator in the app log.
    main_loop_meter::start();

    automation::server::start(port, op_tx, heartbeat);
    tracing::debug!("[agent] in-process element automation server on port {port}");
}

// ---------------------------------------------------------------------------
// Active-window cell registry (e2e only)
//
// The test-command drain (`handle_test_command`) reads the active main
// window through a set of `Rc<RefCell<Option<_>>>` cells created in
// `start_test_agent_if_enabled`. The set_state-driven onboarding→main
// transition (inside `handle_test_command`) re-points those cells at the new
// window, so state-protocol nav keeps working after it. But the *real*
// onboarding path builds its window in
// `views::onboarding::launch_main_app_after_signin` — outside the drain — and
// historically left the cells untouched, so `current_stack` stayed `None` and
// `navigate_to(...)` became a silent no-op (the believable live-mail e2e
// reached the feed but couldn't reach mail-settings / conversations).
//
// This registry holds clones of those same `Rc` cells (GTK is single-threaded
// → a `thread_local` is the right home; the cells are `!Send`). The real
// onboarding transition calls `rebind_active_window` to re-point them, so both
// transition paths converge on one wiring. Populated only in e2e mode (the
// registering call site returns early when no agent/bridge is configured), so
// it is inert for real users.
// ---------------------------------------------------------------------------

/// Rebuild the Profile page for a target and show it — `None` = the viewer's own
/// profile, `Some(hex)` = another actor's. See
/// [`app::MainWindowResult::open_profile`] for why reaching a specific actor's
/// profile is a rebuild rather than a stack switch.
type OpenProfileFn = Rc<dyn Fn(Option<String>)>;

/// The active main window's [`OpenProfileFn`], re-pointed at each of the four
/// sites that hand a freshly built main window to the test-command drain.
type OpenProfileCell = Rc<std::cell::RefCell<Option<OpenProfileFn>>>;

#[derive(Clone)]
struct ActiveWindowCells {
    stack: Rc<std::cell::RefCell<Option<gtk::Stack>>>,
    app_state: Rc<std::cell::RefCell<Option<Rc<std::cell::RefCell<app::AppState>>>>>,
    error_label: Rc<std::cell::RefCell<Option<gtk::Label>>>,
    warning_label: Rc<std::cell::RefCell<Option<gtk::Label>>>,
    info_label: Rc<std::cell::RefCell<Option<gtk::Label>>>,
    client: Rc<std::cell::RefCell<Option<Rc<client::FaunaClient>>>>,
    onboarding_machine:
        Rc<std::cell::RefCell<Option<std::sync::Arc<fauna_onboarding_machine::OnboardingMachine>>>>,
    open_profile: OpenProfileCell,
}

thread_local! {
    static ACTIVE_WINDOW_CELLS: std::cell::RefCell<Option<ActiveWindowCells>> =
        const { std::cell::RefCell::new(None) };
}

fn register_active_window_cells(cells: ActiveWindowCells) {
    ACTIVE_WINDOW_CELLS.with(|c| *c.borrow_mut() = Some(cells));
}

/// Re-point the test-command drain's cells at a main window built outside the
/// activate / `handle_test_command` path — specifically the real-onboarding
/// transition in `views::onboarding::launch_main_app_after_signin`. Without
/// this, state-protocol nav (`navigate_to`) is a silent no-op after real
/// onboarding because only the set_state-driven transition updates the cells.
/// A no-op when the registry is empty (real users — the agent isn't wired).
pub fn rebind_active_window(
    stack: gtk::Stack,
    app_state: Rc<std::cell::RefCell<app::AppState>>,
    error_label: gtk::Label,
    warning_label: gtk::Label,
    info_label: gtk::Label,
    client: Rc<client::FaunaClient>,
    open_profile: OpenProfileFn,
) {
    ACTIVE_WINDOW_CELLS.with(|c| {
        if let Some(cells) = c.borrow().as_ref() {
            *cells.stack.borrow_mut() = Some(stack);
            *cells.app_state.borrow_mut() = Some(app_state);
            *cells.error_label.borrow_mut() = Some(error_label);
            *cells.warning_label.borrow_mut() = Some(warning_label);
            *cells.info_label.borrow_mut() = Some(info_label);
            *cells.client.borrow_mut() = Some(client);
            *cells.open_profile.borrow_mut() = Some(open_profile);
        }
    });
}

/// Re-point the test agent's onboarding-machine cell at a wizard built outside
/// the startup drain — specifically the append ("Add account") wizard launched
/// from a running authenticated session (`launch_add_account_wizard`). The test
/// agent's `call_machine_method` (the handle-check / invite snapshot injectors)
/// drives whichever machine this cell holds; without re-pointing it, an append
/// wizard is undrivable because the cell still holds the stale startup machine
/// (`None` when the app launched authenticated). A no-op for real users (the
/// agent isn't wired). Mirrors `rebind_active_window` for the machine cell.
fn set_active_onboarding_machine(
    machine: std::sync::Arc<fauna_onboarding_machine::OnboardingMachine>,
) {
    ACTIVE_WINDOW_CELLS.with(|c| {
        if let Some(cells) = c.borrow().as_ref() {
            *cells.onboarding_machine.borrow_mut() = Some(machine);
        }
    });
}

/// Re-point the test agent's error-label cell at an onboarding window built
/// outside the activate / `handle_test_command` path — specifically the fresh
/// wizard `register_sign_out_handler` hands the user. Mirrors
/// [`set_active_onboarding_machine`] for the label cell, and no-op for real
/// users (the agent isn't wired).
///
/// ⚠ **A stale cell here does not merely lose ONE line — it blinds the state
/// protocol to that surface for the rest of the process.** `update_shared_state`
/// serializes whatever label this cell holds, so after a sign-out re-pointed
/// nothing the agent kept reporting the *closed* window's label: the sign-out
/// residue line was painted where the user could see
/// it and read back as `''`, and so would every later onboarding error. Found by
/// that row's own e2e, which is the only reason it is not still true — the
/// element-level reads (`create-identity-button`) resolve by widget name and
/// were never affected, so nothing else noticed.
fn set_active_error_label(label: gtk::Label) {
    ACTIVE_WINDOW_CELLS.with(|c| {
        if let Some(cells) = c.borrow().as_ref() {
            *cells.error_label.borrow_mut() = Some(label);
        }
    });
}

/// Forget the authenticated shell the test agent's cells point at — the
/// production sign-out's twin of the e2e `reset` arm's own cell clears
/// (`handle_test_command`, `"reset"`), for the window `register_sign_out_handler`
/// closes. No-op for real users (the agent isn't wired).
///
/// ⚠ **Without this a session patch after a UI sign-out is a DROPPED command**
/// (e2e-conventions.md convention 11): the patch arm builds a main window only
/// while `current_stack` reads `None`, and the sign-out handler closed the
/// window without telling the agent — so a `set_state({session})` that follows
/// a real sign-out took the "same-actor re-establish" branch against a shell
/// that no longer existed, and the app sat at onboarding with nothing logged.
/// Every earlier journey signed back in through the fixture's `reset`, which
/// clears the cells itself, so the gap stayed invisible until
/// `test_sign_out_device_roster.py` drove sign-out → sign-in through the UI
/// gesture alone (2026-09-14). The client and app-state handles are dropped
/// here only as *cell* contents: their lifetimes belong to the widget tree the
/// handler is tearing down, exactly as before.
fn retire_active_window() {
    ACTIVE_WINDOW_CELLS.with(|c| {
        if let Some(cells) = c.borrow().as_ref() {
            *cells.stack.borrow_mut() = None;
            *cells.app_state.borrow_mut() = None;
            *cells.client.borrow_mut() = None;
            *cells.open_profile.borrow_mut() = None;
        }
    });
}

/// Release builds without `e2e-agent` compile no automation surface at all
/// (testing.md convention 15) — every call site below stays unchanged, this
/// is simply inert.
#[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
#[allow(clippy::too_many_arguments)]
fn start_test_agent_if_enabled(
    _application: &adw::Application,
    _stack: Option<gtk::Stack>,
    _app_state: Option<Rc<std::cell::RefCell<app::AppState>>>,
    _error_label: Option<gtk::Label>,
    _warning_label: Option<gtk::Label>,
    _info_label: Option<gtk::Label>,
    _fauna_client: Option<Rc<client::FaunaClient>>,
    _onboarding_machine: Option<std::sync::Arc<fauna_onboarding_machine::OnboardingMachine>>,
    _open_profile: Option<OpenProfileFn>,
) {
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
#[allow(clippy::too_many_arguments)]
fn start_test_agent_if_enabled(
    application: &adw::Application,
    stack: Option<gtk::Stack>,
    app_state: Option<Rc<std::cell::RefCell<app::AppState>>>,
    error_label: Option<gtk::Label>,
    warning_label: Option<gtk::Label>,
    info_label: Option<gtk::Label>,
    fauna_client: Option<Rc<client::FaunaClient>>,
    onboarding_machine: Option<std::sync::Arc<fauna_onboarding_machine::OnboardingMachine>>,
    open_profile: Option<OpenProfileFn>,
) {
    // In-process element-automation server (replaces the AT-SPI bridge's
    // /element/* surface). Independent of FAUNA_E2E_BRIDGE so it can run
    // standalone during migration; started once for the app's lifetime.
    start_element_agent_once(application);

    // Enabled by either transport: the AT-SPI bridge (poll loop) or the
    // in-process agent server (serves /app/* from the link installed below).
    let bridge_url = std::env::var("FAUNA_E2E_BRIDGE").ok();
    let agent_enabled = std::env::var_os("FAUNA_E2E_AGENT_PORT").is_some();
    if bridge_url.is_none() && !agent_enabled {
        return;
    }
    if let Some(ref url) = bridge_url {
        tracing::debug!("[TestAgent] Bridge URL: {url}");
    }

    use std::cell::RefCell;
    use std::sync::{Arc, Mutex};

    // Mutable refs wrapped in Rc<RefCell<>> so the command handler can
    // swap them when transitioning from onboarding → main window.
    let current_stack: Rc<RefCell<Option<gtk::Stack>>> = Rc::new(RefCell::new(stack));
    let current_app_state: Rc<RefCell<Option<Rc<RefCell<app::AppState>>>>> =
        Rc::new(RefCell::new(app_state));
    let current_error_label: Rc<RefCell<Option<gtk::Label>>> = Rc::new(RefCell::new(error_label));
    let current_warning_label: Rc<RefCell<Option<gtk::Label>>> =
        Rc::new(RefCell::new(warning_label));
    let current_info_label: Rc<RefCell<Option<gtk::Label>>> = Rc::new(RefCell::new(info_label));
    let current_client: Rc<RefCell<Option<Rc<client::FaunaClient>>>> =
        Rc::new(RefCell::new(fauna_client));
    let current_onboarding_machine: Rc<
        RefCell<Option<std::sync::Arc<fauna_onboarding_machine::OnboardingMachine>>>,
    > = Rc::new(RefCell::new(onboarding_machine));
    // The active main window's profile-page opener (`app.rs`'s `open_profile`).
    // Travels with `current_stack`/`current_client` through all four
    // window-hand-over sites so a `{"view":"profile","actor_id":…}` nav can
    // rebuild the page for its target instead of silently showing SELF.
    let current_open_profile: OpenProfileCell = Rc::new(RefCell::new(open_profile));

    // Register clones of the same cells so the real-onboarding transition
    // (`launch_main_app_after_signin`, built outside this drain) can re-point
    // them via `rebind_active_window` — keeping state-protocol nav working
    // after real onboarding, not just the set_state-driven transition below.
    register_active_window_cells(ActiveWindowCells {
        stack: current_stack.clone(),
        app_state: current_app_state.clone(),
        error_label: current_error_label.clone(),
        warning_label: current_warning_label.clone(),
        info_label: current_info_label.clone(),
        client: current_client.clone(),
        onboarding_machine: current_onboarding_machine.clone(),
        open_profile: current_open_profile.clone(),
    });

    let shared = Arc::new(Mutex::new(test_agent::SharedState::default()));

    // Seed session override from keyring once at startup (one D-Bus call).
    // After this, update_shared_state never touches the keyring.
    // `authenticated` is deliberately NOT seeded: stored credentials are not a
    // session. The launch machine may route a credentialed relaunch to the
    // wizard (verify 404 → invite_request/claim_code), where "authenticated":
    // true would be a lie — `update_shared_state` derives the flag from
    // whether the authenticated main window is actually mounted, matching the
    // other apps' agents.
    if let Some((url, hex, did)) = launch_credentials() {
        let mut s = shared.lock().unwrap();
        s.session_override = Some(test_agent::SessionOverride {
            node_url: Some(url),
            secret_hex: Some(hex),
            device_id: Some(did),
            ..Default::default()
        });
    }

    // --- Initial state push ---
    update_shared_state(
        &shared,
        &current_stack.borrow(),
        &current_app_state.borrow(),
        &current_error_label.borrow(),
    );

    // --- Track stack page changes (if we have a stack) ---
    if let Some(ref s) = *current_stack.borrow() {
        let shared_for_stack = shared.clone();
        let cs = current_stack.clone();
        let cas = current_app_state.clone();
        let cel = current_error_label.clone();
        let cwl = current_warning_label.clone();
        let cil = current_info_label.clone();
        s.connect_visible_child_name_notify(move |_| {
            // Clear page-scoped banners on navigation
            for lbl_cell in [&cel, &cwl, &cil] {
                if let Some(ref lbl) = *lbl_cell.borrow() {
                    lbl.set_visible(false);
                    lbl.set_text("");
                }
            }
            update_shared_state(
                &shared_for_stack,
                &cs.borrow(),
                &cas.borrow(),
                &cel.borrow(),
            );
        });
    }

    // Command channel shared by both transports. The GTK drain below consumes
    // it; the bridge poll loop and/or the in-process agent server feed it.
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<test_agent::RawCommand>();

    // Expose this stage's state link to the in-process agent server so it can
    // serve /app/{state,commands} (replaces the bridge relaying them).
    automation::link::install(shared.clone(), cmd_tx.clone());

    // --- Start background polling thread (only when a bridge URL is set) ---
    if let Some(url) = bridge_url {
        test_agent::start(url, shared.clone(), cmd_tx);
    }

    // --- GTK main-thread command processing (50ms tick) ---
    let app_ref = application.clone();
    let shared_for_poll = shared.clone();
    let cs_for_poll = current_stack.clone();
    let cas_for_poll = current_app_state.clone();
    let cel_for_poll = current_error_label.clone();
    let cwl_for_poll = current_warning_label.clone();
    let cil_for_poll = current_info_label.clone();
    let cc_for_poll = current_client.clone();
    let com_for_poll = current_onboarding_machine.clone();
    let cop_for_poll = current_open_profile.clone();

    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        let mut cmd_count = 0u32;
        main_loop_meter::drain("agent-commands", |tick| {
            while let Ok(cmd) = cmd_rx.try_recv() {
                cmd_count += 1;
                tick.item(
                    || cmd.action.clone(),
                    || {
                        handle_test_command(
                            &cmd,
                            &app_ref,
                            &cs_for_poll,
                            &cas_for_poll,
                            &shared_for_poll,
                            &cel_for_poll,
                            &cwl_for_poll,
                            &cil_for_poll,
                            &cc_for_poll,
                            &com_for_poll,
                            &cop_for_poll,
                        )
                    },
                );
            }
        });
        if cmd_count > 0 {
            tracing::debug!("[TestAgent-GTK] Processed {cmd_count} commands");
        }
        // Refresh state every tick so the bridge cache stays fresh.
        main_loop_meter::dispatch(
            "agent-state-publish",
            || "tick".to_string(),
            || {
                update_shared_state(
                    &shared_for_poll,
                    &cs_for_poll.borrow(),
                    &cas_for_poll.borrow(),
                    &cel_for_poll.borrow(),
                )
            },
        );
        glib::ControlFlow::Continue
    });
}

/// The e2e `reset`/`logout` arm's half that follows the account runtime's
/// stop — the agent's emulation of the production sign-out-all
/// (`sign_out_after_stop`), run as `reset_actor_scoped_state`'s continuation.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
#[allow(clippy::too_many_arguments)]
fn reset_after_stop(
    application: &adw::Application,
    current_stack: &Rc<std::cell::RefCell<Option<gtk::Stack>>>,
    current_app_state: &Rc<std::cell::RefCell<Option<Rc<std::cell::RefCell<app::AppState>>>>>,
    shared: &std::sync::Arc<std::sync::Mutex<test_agent::SharedState>>,
    current_error_label: &Rc<std::cell::RefCell<Option<gtk::Label>>>,
    current_client: &Rc<std::cell::RefCell<Option<Rc<client::FaunaClient>>>>,
    current_onboarding_machine: &Rc<
        std::cell::RefCell<Option<std::sync::Arc<fauna_onboarding_machine::OnboardingMachine>>>,
    >,
) {
    if let Ok(mut s) = shared.lock() {
        s.session_override = None;
        // The one clear of the agent-failure slot (convention 11): every
        // `app` fixture resets before a test body, so each test gets
        // exactly one clean read of it and a failure can never leak
        // forward into a test that did not cause it.
        s.agent_command_failure = None;
    }
    // Transition back to onboarding: tear down the authenticated main
    // window, then show onboarding. This tears down via `destroy()`
    // (below), which bypasses `connect_close_request` entirely — so the
    // handler's `app.quit()` never runs and the test agent's poll loop
    // survives the reset. Do NOT set SIGNING_OUT to route the handler
    // here: the handler does not fire on this path, so the flag would
    // never be consumed by its `swap(false)` and would latch `true`
    // into the *next* real window close — which then takes the sign-out
    // branch, skips `app.quit()`, and leaves the process alive on the
    // leaked `app.hold()`.
    if current_stack.borrow().is_some() {
        // The UI pump already stopped before the account runtime's stop was
        // handed off (the `reset` arm), as on every production teardown.
        // The external sync agent is already un-provisioned — its reply is
        // part of that stop (`account_runtime::teardown`): it stopped its
        // engines (releasing their inotify instances in the *agent* process)
        // and deleted the persisted capability, so the next test's
        // re-auth re-provisions from scratch. The agent process itself
        // survives the reset — production semantics; under e2e it is a
        // direct-spawned child inside this launch's isolated XDG
        // world, reaped with the harness's process group.
        tray::TRAY_COMPOSE.store(false, Ordering::SeqCst);
        // Destroy (not close) every app-registered window. `close()`
        // fires `connect_close_request`, whose close-to-tray path can
        // merely hide the window — leaving the whole widget tree
        // (stack → views → their `Rc<FaunaClient>` clones) alive, so the
        // authenticated client never drops and its runtime/fds leak
        // across the suite's reset→re-auth-per-test cycle. `destroy()`
        // bypasses the handler and frees the tree, matching the
        // canonical sign-out paths (`register_sign_out_handler`).
        for win in application.windows() {
            win.destroy();
        }
        // We destroy() (bypasses connect_close_request), so no
        // SIGNING_OUT dance is needed for teardown; clear it so the
        // incoming window's close handler behaves normally (honors the
        // close-to-tray pref, and quits on the real close). Same
        // invariant as the account-switch teardown above: a
        // destroy()-based path leaves this flag false.
        tray::SIGNING_OUT.store(false, Ordering::SeqCst);
        *current_stack.borrow_mut() = None;
        // The AppState does not actually drop here — the distributed
        // widget-tree signal-closure cycle keeps `Rc<RefCell<AppState>>`
        // clones alive — but post-cutover it owns no per-test resource
        // handles: the resident sync engines (and their inotify
        // instances) live in the external agent process, already
        // stopped by the un-provision this continuation waited for. (`mls`/`p2p`
        // are `Arc`-shared with the settings thread-locals +
        // conversations rail; they hold few fds and don't gate the
        // suite.)
        *current_app_state.borrow_mut() = None;
        // Tear down the outgoing client's background runtime *now*,
        // before dropping our `Rc` handle. The authenticated window's
        // widget tree does not finalize on `destroy()` (a distributed
        // GTK-rs signal-closure reference cycle keeps ~100 `Rc<FaunaClient>`
        // clones alive — tracked internally), so
        // relying on `Drop` to stop the background work never fires.
        // `shutdown()` aborts the runtime's tasks (WS reconnect, inbound
        // poll, sync engine + inotify watcher, P2P) immediately, which is
        // what actually starves the GTK main thread / exhausts inotify
        // instances across the suite's reset→re-auth-per-test cycle.
        if let Some(c) = current_client.borrow().as_ref() {
            c.shutdown();
        }
        *current_client.borrow_mut() = None;
        // Erase LAST — every writer above is down, so nothing can land a
        // credential back into the namespace after the sweep passes over
        // it. This is the ordering half of `long-term-store.md`
        // § Cleanup contract ("drop the launch machine, the client, and
        // any registry reader first; erase last"); the other half is that
        // `AccountRegistry`'s reads no longer write, so the launch
        // machine's un-cancellable worker (`async_helper::run_on_tokio`
        // spawns a bare `std::thread` — `c.shutdown()` cannot reach it)
        // can no longer resurrect the identity if it lands mid-wipe.
        // Erase before building the onboarding window, which constructs a
        // fresh registry.
        //
        // Account-scoped local state erases too, and it MUST — this arm
        // emulates the production sign-out-all at
        // `register_sign_out_handler`, which erases the scopes before it
        // wipes the credentials for a reason that is invisible until a
        // SECOND login: the W3 (account-data-plane.md § Workstreams) account store's writer key lives in the
        // T10 credential slot, but the store itself lives on disk under
        // `<config>/fauna/sync/<actor>/`. Wiping only the slot leaves the
        // store carrying the old writer identity, so the next login
        // mints a fresh key and the store refuses it forever ("belongs
        // to a different writer") — the app then runs with NO account
        // runtime for the rest of the process, every store-backed
        // surface failing, nothing on screen saying why.
        // Measured 2026-08-18: every reset in
        // the suite was producing a state production never produces.
        // Ordering matches the production path — the registry this reads
        // to know which actors existed is what `delete_credentials`
        // wipes, so it runs first.
        // Dropped deliberately, and this is the one place it is right
        // to: an agent reset has no user in front of it, and painting a
        // residue line here would make every test's own between-test
        // reset look like a failed sign-out. The production path above
        // is where a survivor owes the user a say; the sweeps still log
        // their paths either way.
        let _ = account_scope::erase_all_known_accounts();
        let _ = client::delete_credentials();
        let onboarding = views::onboarding::build_onboarding_window(application);
        *current_error_label.borrow_mut() = Some(onboarding.error_label);
        *current_onboarding_machine.borrow_mut() = Some(onboarding.machine);
        show_window(&onboarding.window);
    } else {
        // No authenticated shell mounted: there is no client, pump or
        // backup coordinator to quiesce, so the erase is already last.
        // The scope erase is owed here too — a reset from onboarding
        // must not leave a previous run's account store behind for the
        // next login to mint a second writer key against (same reason
        // as the authenticated arm above).
        // Dropped for the same reason as the arm above — no user, no
        // line.
        let _ = account_scope::erase_all_known_accounts();
        let _ = client::delete_credentials();
        if let Some(m) = current_onboarding_machine.borrow().as_ref() {
            // Already in the onboarding wizard — rewind the
            // OnboardingMachine to IdentityChoice so the next E2E test
            // starts on the canonical first stage. The observer fires,
            // the orchestrator swaps the stack page, and the existing
            // window stays alive (closing it would trigger the wizard's
            // close_request handler and call app.quit()).
            m.reset();
        }
    }
}

// ---------------------------------------------------------------------------
// Command handler — the single place that processes all test commands.
// ---------------------------------------------------------------------------

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
#[allow(clippy::too_many_arguments)]
fn handle_test_command(
    cmd: &test_agent::RawCommand,
    application: &adw::Application,
    current_stack: &Rc<std::cell::RefCell<Option<gtk::Stack>>>,
    current_app_state: &Rc<std::cell::RefCell<Option<Rc<std::cell::RefCell<app::AppState>>>>>,
    shared: &std::sync::Arc<std::sync::Mutex<test_agent::SharedState>>,
    current_error_label: &Rc<std::cell::RefCell<Option<gtk::Label>>>,
    current_warning_label: &Rc<std::cell::RefCell<Option<gtk::Label>>>,
    current_info_label: &Rc<std::cell::RefCell<Option<gtk::Label>>>,
    current_client: &Rc<std::cell::RefCell<Option<Rc<client::FaunaClient>>>>,
    current_onboarding_machine: &Rc<
        std::cell::RefCell<Option<std::sync::Arc<fauna_onboarding_machine::OnboardingMachine>>>,
    >,
    current_open_profile: &OpenProfileCell,
) {
    let started = std::time::Instant::now();
    tracing::debug!("[TestAgent] handle_test_command: action={}", cmd.action);
    match cmd.action.as_str() {
        "reset" | "logout" => {
            // NB: the credential wipe is deliberately NOT here. It runs last, in
            // both branches below, once the writers are down — see the comments
            // at each call and `long-term-store.md` § Cleanup contract.
            //
            // Between-test isolation: wipe the singleton ConversationsManager
            // (threads, drafts, selection, search, sort). Without this, the
            // OnceLock-backed manager carries state across pytest's per-test
            // `driver.reset()` cycle and tests see threads from earlier
            // tests, breaking thread-count and key-merge assertions.
            crate::conversations::manager().clear_for_test();
            // A barrier probe token is scoped to ONE test — same lifetime and
            // same clear point as tui's `App::barrier_probe`.
            automation::link::clear_barrier_probe();
            // Drop stale snapshot observers from the outgoing window. The linux
            // authenticated window's widget tree does not finalize on
            // `destroy()` (a distributed GTK signal-closure cycle), so a prior
            // window's leaked conversations panes otherwise linger in the a11y
            // tree and field clicks (e.g. `add-participant-confirm`) against the
            // client whose runtime this sign-out is about to shut down — the
            // task is silently dropped (`rt.spawn` on a dead runtime) and the
            // membership mutation never runs. Clearing observers closes each
            // stale loop's channel so it breaks and releases its panes; the next
            // login re-attaches a live observer.
            crate::conversations::manager().clear_observers();
            // Every other actor-scoped drop, including the feed/search
            // process-wide manager slots (`manager()` has no actor key, so
            // leaving the outgoing actor's installed lets a post-reset reader
            // — the E2E state serializer's posts arm — hand out the previous
            // identity's post list). Unconditional, unlike the window teardown
            // below: "reset" means the next actor starts clean whether or not
            // an authenticated stack is currently mounted.
            // The UI pump stops NOW, before the stop is handed off the thread,
            // as on every production teardown (`quiesce_for_teardown`) — the
            // outgoing client must not repaint what the reset just dropped
            // while the stop runs.
            if current_stack.borrow().is_some() {
                settings::trigger_pump_shutdown();
            }
            // Everything else, and the ACK, follow the stop. The erase must never
            // meet a store still open (`apps/account-scoping.md` § Erasure
            // follows scope), and an ack sent before it would let the next
            // test's login race that erase — so the command stays un-acked, and
            // the driver waiting, until the continuation has run. The GTK
            // thread meanwhile goes back to the main loop.
            let application = application.clone();
            let current_stack = current_stack.clone();
            let current_app_state = current_app_state.clone();
            let shared = std::sync::Arc::clone(shared);
            let current_error_label = current_error_label.clone();
            let current_client = current_client.clone();
            let current_onboarding_machine = current_onboarding_machine.clone();
            let id = cmd.id.clone();
            crate::actor_scope::reset_actor_scoped_state(
                fauna_client_account_runtime::StopReason::SignOut,
                move || {
                    reset_after_stop(
                        &application,
                        &current_stack,
                        &current_app_state,
                        &shared,
                        &current_error_label,
                        &current_client,
                        &current_onboarding_machine,
                    );
                    ack_applied_command(
                        &shared,
                        &current_stack,
                        &current_app_state,
                        &current_error_label,
                        &id,
                    );
                },
            );
            return;
        }
        "patch" => {
            let Some(ref state) = cmd.state else { return };

            // --- Session patch ---
            if let Some(session) = state.get("session") {
                // Update session overrides so state serialization reflects
                // what the test set, even before credentials are in the keyring.
                {
                    let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                    let ov = s.session_override.get_or_insert_with(Default::default);
                    if let Some(v) = session.get("authenticated").and_then(|v| v.as_bool()) {
                        ov.authenticated = Some(v);
                    }
                    if let Some(v) = session.get("node_url").and_then(|v| v.as_str()) {
                        ov.node_url = Some(v.to_string());
                    }
                    if let Some(v) = session.get("secret_hex").and_then(|v| v.as_str()) {
                        ov.secret_hex = Some(v.to_string());
                    }
                    if let Some(v) = session.get("actor_id").and_then(|v| v.as_str()) {
                        ov.actor_id = Some(v.to_string());
                    }
                    if let Some(v) = session.get("handle").and_then(|v| v.as_str()) {
                        ov.handle = Some(v.to_string());
                    }
                    if let Some(v) = session.get("device_id").and_then(|v| v.as_str()) {
                        ov.device_id = Some(v.to_string());
                    }
                }

                // --- Actor switch: tear the authenticated shell down first ---
                // The shell-build branch below is gated on `current_stack…
                // .is_none()`, which is false whenever an authenticated window
                // is already mounted. So a second `set_state({session})` naming
                // a DIFFERENT actor used to be silently HALF-applied: the
                // override above, the keyring write and the account cache all
                // became actor B, while every live object — `FaunaClient`, its
                // `NestClient`, `feed::host`'s manager, the conversations
                // manager — stayed actor A's. `get_state` then reported B while
                // the app drove A, which is not a missing feature but a
                // wrong-data bug: on `test_gated_post_compose.py`'s subscriber
                // leg the author's own custody unsealed the gated body into the
                // "subscriber's" list card, so the teaser-only assertion failed
                // by LEAKING the full body; once the freshly stored credentials
                // reached the live client's token refresh, the same run could
                // instead read an empty feed — two symptoms, one dropped
                // command, and the reason that test was flaky-red on main
                // rather than honestly red. E2E convention 11 (a test agent
                // must honour a command or fail loudly, never drop it) makes
                // honouring it the fix.
                //
                // Honour it by reducing "second login" to "reset, then login" —
                // the path that always worked (`driver.reset()` navigates back
                // to onboarding, so the build branch's guard is true and the
                // whole shell is rebuilt). Same teardown as the "reset" arm
                // above, with two deliberate differences: no
                // `delete_credentials()` (the new actor's are stored just below,
                // and `launch_authenticated` is handed them explicitly), and no
                // onboarding window (the build branch takes over immediately).
                // Gated on `authenticated == true` so we only ever tear down
                // when the branch below is certain to rebuild.
                let actor_switch = match current_client.borrow().as_ref() {
                    Some(live) => session_patch_switches_session(
                        session,
                        &live.secret_bytes(),
                        live.node_url(),
                    ),
                    None => false,
                };
                if actor_switch {
                    tracing::info!(
                        "[TestAgent] session patch names a different actor than the live \
                         session — tearing the authenticated shell down so it rebuilds"
                    );
                    // Identity-scoped singletons: both survive a window
                    // teardown, so a switch that skipped these would carry the
                    // outgoing actor's threads/posts into the incoming one.
                    // `clear_for_test` is e2e-only (absent from a release
                    // build), so it stays at this site; everything a production
                    // actor change drops lives in `actor_scope`.
                    crate::conversations::manager().clear_for_test();
                    crate::conversations::manager().clear_observers();
                    settings::trigger_pump_shutdown();
                    // The rest of the teardown follows the account runtime's
                    // stop, which runs off this thread — and so does the
                    // command itself: once the shell is down the patch is
                    // simply re-dispatched, where the live-client check now
                    // reads `false` and the build branch below rebuilds as the
                    // new actor. The ack is that re-dispatch's, so the driver
                    // waits across the stop exactly as for `reset`.
                    let cmd = cmd.clone();
                    let application = application.clone();
                    let current_stack = current_stack.clone();
                    let current_app_state = current_app_state.clone();
                    let shared = std::sync::Arc::clone(shared);
                    let current_error_label = current_error_label.clone();
                    let current_warning_label = current_warning_label.clone();
                    let current_info_label = current_info_label.clone();
                    let current_client = current_client.clone();
                    let current_onboarding_machine = current_onboarding_machine.clone();
                    let current_open_profile = current_open_profile.clone();
                    crate::actor_scope::reset_actor_scoped_state(
                        fauna_client_account_runtime::StopReason::AccountSwitch,
                        move || {
                            tray::TRAY_COMPOSE.store(false, Ordering::SeqCst);
                            for win in application.windows() {
                                win.destroy();
                            }
                            tray::SIGNING_OUT.store(false, Ordering::SeqCst);
                            *current_stack.borrow_mut() = None;
                            *current_app_state.borrow_mut() = None;
                            // Stop the outgoing client's background runtime before
                            // dropping our handle — the widget tree does not finalize on
                            // `destroy()` (the GTK-rs signal-closure cycle), so `Drop`
                            // never runs and its WS reconnect / poll / sync tasks would
                            // keep racing the incoming actor's.
                            if let Some(c) = current_client.borrow().as_ref() {
                                c.shutdown();
                            }
                            *current_client.borrow_mut() = None;
                            handle_test_command(
                                &cmd,
                                &application,
                                &current_stack,
                                &current_app_state,
                                &shared,
                                &current_error_label,
                                &current_warning_label,
                                &current_info_label,
                                &current_client,
                                &current_onboarding_machine,
                                &current_open_profile,
                            );
                        },
                    );
                    return;
                }

                // Store credentials if provided.
                if let (Some(url), Some(hex)) = (
                    session.get("node_url").and_then(|v| v.as_str()),
                    session.get("secret_hex").and_then(|v| v.as_str()),
                ) {
                    let device_id = session
                        .get("device_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("test-device");
                    // The session patch is a registry write, exactly the shape a
                    // real sign-in leaves behind: `add_account` enrolls the
                    // identity and writes its per-actor slots (an idempotent
                    // upsert) and `set_active` moves the pointer. On an actor
                    // switch that pointer move is load-bearing: the rebuild
                    // below hands the fresh `LaunchMachine` a
                    // `launch_persistence()` whose `load_identity()` resolves
                    // `registry.active()`, so leaving it on the OUTGOING actor
                    // mints the silent-challenge bearer for actor A while
                    // `launch_authenticated` builds the `FaunaClient` around the
                    // incoming `hex` (actor B) — a pairing the nest refuses at the
                    // WS handshake (`security.md` § Cross-connection binding) →
                    // `403 Forbidden`, retried forever, so every WS-RPC call HANGS
                    // and the UI reads "0 posts, error-message=''". A fresh
                    // account is never `require_confirm_to_activate`, so plain
                    // `set_active` is right here; the Stage-2 re-auth gate has no
                    // meaning for a test-injected identity. Test-agent only, like
                    // the whole handler.
                    //
                    // ⚠ Never gate this write on `actor_switch`. An actor switch
                    // reaches this line only in its RE-DISPATCH, after the shell
                    // is down, where `current_client` is `None` and `actor_switch`
                    // therefore reads `false`. While the write sat under
                    // `if actor_switch`, the switch's teardown-then-re-dispatch
                    // skipped it every time: the machine minted for the outgoing
                    // actor and every second login inside one app lost its nest
                    // connection.
                    {
                        let registry = account_registry();
                        match registry.add_account(hex, Some(url), Some(device_id)) {
                            Ok(actor_id) => {
                                if let Err(e) = registry.set_active(&actor_id) {
                                    tracing::error!(
                                        "[TestAgent] set_active({actor_id}) failed on the \
                                         session patch: {e:#} — the rebuilt session will 403 \
                                         forever"
                                    );
                                }
                            }
                            Err(e) => tracing::error!(
                                "[TestAgent] add_account failed on the session patch: {e:#} \
                                 — the rebuilt session will 403 forever"
                            ),
                        }
                    }

                    // ...and adopt the patch's device id as this device's REAL sync
                    // identity too, not just the registry credential slots
                    // above — `devices.md` § This-device marker needs one value to
                    // do both jobs (`crate::sync::adopt_device_id_hex`'s doc comment).
                    // Must run AFTER the actor-switch block above: `sync_state_dir()`
                    // resolves the ACTIVE actor, which on a switch only becomes the
                    // incoming one once `set_active` (just above) has run.
                    if let Some(real_device_id) = session.get("device_id").and_then(|v| v.as_str())
                        && !crate::sync::adopt_device_id_hex(real_device_id)
                    {
                        tracing::warn!("[TestAgent] session patch: could not adopt the device id");
                    }
                }

                // Seed the account cache with the injected `<handle>@<domain>`.
                // The state protocol logs a user in without onboarding, so the
                // nest holds no handle for this actor and `silent_sign_in`'s
                // verify returns `""` — leaving the cache handle-less. Anything
                // keyed on the logged-in user's address (e.g. the conversations
                // session's self-address seed at AuthSuccess) would then come
                // up empty. Writing it here mirrors
                // a real onboarded user's cache. Both the handle and domain are
                // written *synchronously*: the launch flow's domain write
                // (`silent_sign_in` / the `LaunchMachine` silent challenge) is
                // async and would otherwise race the first send. The domain is
                // the nest host (the verify domain isn't on the wire here); its
                // exact value is cosmetic since out-of-domain From addresses
                // bypass the nest's from-handle check. Test-agent only — this
                // whole handler runs solely under `FAUNA_E2E_BRIDGE`.
                if let Some(handle) = session
                    .get("handle")
                    .and_then(|v| v.as_str())
                    .filter(|h| !h.is_empty())
                {
                    let domain = session
                        .get("node_url")
                        .and_then(|v| v.as_str())
                        .map(fauna_core::format::url_host)
                        .filter(|d| !d.is_empty())
                        .unwrap_or_else(|| "localhost".to_string());
                    let _ = client::store_account_cache(Some(handle), Some(&domain), None);
                }

                // If authenticated=true and we don't have a main window yet,
                // transition from onboarding → main window.
                if session.get("authenticated").and_then(|v| v.as_bool()) == Some(true)
                    && current_stack.borrow().is_none()
                {
                    if let (Some(url), Some(hex)) = (
                        session.get("node_url").and_then(|v| v.as_str()),
                        session.get("secret_hex").and_then(|v| v.as_str()),
                    ) {
                        let url = url.to_string();
                        let hex = hex.to_string();
                        // Tear down the onboarding window without firing
                        // close-request — its handler calls `app.quit()`
                        // (added so the user's X click quits),
                        // and a programmatic close here would kill the app
                        // before we can build the main window. Matches the
                        // `onboarding_window.destroy()` pattern used by
                        // `launch_main_app_after_signin`.
                        if let Some(win) = application.active_window() {
                            win.destroy();
                        }
                        // Build the main window. Don't start a second test
                        // agent — just update our Rc refs so the existing
                        // poll loop drives the new window.
                        // LaunchMachine: construct fresh + drive start() so it
                        // reaches Online before FaunaClient consumes it for
                        // token refresh.
                        //
                        // This one stays SYNCHRONOUS, unlike the post-wizard and
                        // account-switch launches: it runs inside
                        // `handle_test_command`, so the built session must be in
                        // place before the command is acked — a driver that gets
                        // its ack first would read the pre-`set_state` session
                        // and race every assertion after it. What it does NOT do
                        // any more is build and drop the tokio runtime on the GTK
                        // main thread: `block_on_tokio` puts both on a worker
                        // thread and parks this thread only on the result
                        // channel, so `Runtime::drop`'s blocking-pool join (the
                        // unbounded stall — see `async_helper`'s
                        // `drop_runtime_loudly`) can no longer reach the main
                        // loop. The silent challenge itself is still awaited
                        // here, by design.
                        let machine = fauna_launch_machine::LaunchMachine::new(
                            std::sync::Arc::new(fauna_launch_machine::NullObserver),
                            // Registry-backed launch persistence (reads the active
                            // account) —
                            // see docs/goal/architecture/long-term-store.md § Shared seam.
                            std::sync::Arc::new(launch_persistence()),
                        );
                        let machine_for_start = std::sync::Arc::clone(&machine);
                        crate::async_helper::block_on_tokio(async move {
                            machine_for_start.start().await
                        });
                        // Become the incoming account's instance BEFORE the rebuild,
                        // exactly as the account switcher's rebuild does: the Account
                        // page is built inside `launch_authenticated` and keys its
                        // served row on this holder, which would otherwise still name
                        // the outgoing actor until `AuthSuccess` (now `Reused`).
                        if let Ok(keypair) =
                            fauna_core::identity::ActorKeypair::from_secret_hex(&hex)
                        {
                            let actor_id = keypair.actor_id_hex();
                            if let Err(refusal) =
                                crate::account_scope::become_session_instance(&actor_id)
                            {
                                refusal.exit(&actor_id);
                            }
                        }
                        let result = launch_authenticated(application, &url, &hex, machine, |_| {});

                        // Hook up stack tracking on the new window.
                        let shared_for_stack = shared.clone();
                        let cs2 = current_stack.clone();
                        let cas2 = current_app_state.clone();
                        let cel2 = current_error_label.clone();
                        let cwl2 = current_warning_label.clone();
                        let cil2 = current_info_label.clone();
                        result.stack.connect_visible_child_name_notify(move |_| {
                            for lbl_cell in [&cel2, &cwl2, &cil2] {
                                if let Some(ref lbl) = *lbl_cell.borrow() {
                                    lbl.set_visible(false);
                                    lbl.set_text("");
                                }
                            }
                            update_shared_state(
                                &shared_for_stack,
                                &cs2.borrow(),
                                &cas2.borrow(),
                                &cel2.borrow(),
                            );
                        });

                        // Update our mutable refs so subsequent commands
                        // (Navigate, compose-file inject, per-page data
                        // re-fetch) target the new window. Without
                        // `current_client` here, the nav patch's data-refresh
                        // arms and the compose.file handler no-op for the
                        // whole life of a `set_state({session})`-built window.
                        *current_stack.borrow_mut() = Some(result.stack);
                        *current_app_state.borrow_mut() = Some(result.state);
                        *current_error_label.borrow_mut() = Some(result.error_label);
                        *current_warning_label.borrow_mut() = Some(result.warning_label);
                        *current_info_label.borrow_mut() = Some(result.info_label);
                        *current_client.borrow_mut() = Some(result.client);
                        *current_open_profile.borrow_mut() = Some(result.open_profile);

                        show_window(&result.window);
                    }
                } else if session.get("authenticated").and_then(|v| v.as_bool()) == Some(true) {
                    // Same-actor re-establish: the shell is already up (an
                    // actor SWITCH tears it down above, landing in the build
                    // branch instead) — but a re-establish is still meant to
                    // behave like a fresh `session::establish`. tui reaches the
                    // same end by the same means today: its `apply_session_patch`
                    // grew this identical converge arm (it once called
                    // `establish()` unconditionally, which a live `mls_state.db`
                    // no longer allows) and re-fires its own post-auth hooks
                    // here. Without this, a second
                    // `set_state({"session": ...})`
                    // naming the SAME actor was a silent no-op on linux alone —
                    // the universal post-auth feeders (`run_critical_alert_sweep`
                    // and its three siblings below) never re-ran, so
                    // `_reestablish_session`-style e2e patches (this app's only
                    // trigger for a one-shot sweep re-check) could never
                    // observe a change made after the first login. Found by
                    // `test_alert_sweep_directory_feeders_e2e.py`'s linux leg — no rebuild
                    // needed, just re-run the same idempotent, best-effort
                    // hooks `launch_authenticated`'s AuthSuccess path runs.
                    //
                    // The sweep re-runs as ONE pass, never a second loop: the
                    // AuthSuccess loop is still running for this identity, and
                    // re-firing it here stacked one sweeper per re-establish.
                    if let Some(c) = current_client.borrow().as_ref() {
                        c.run_deployment_seed_custody_leg();
                        c.refresh_mail_epoch_schedule();
                        c.run_critical_alert_sweep_once();
                        c.load_muted_keywords();
                    }
                }
            }

            // --- Messages patch (inject error/warning/info into UI) ---
            if let Some(messages) = state.get("messages") {
                if let Some(err_val) = messages.get("error")
                    && let Some(ref label) = *current_error_label.borrow()
                {
                    if let Some(text) = err_val.as_str() {
                        label.set_text(text);
                        label.set_visible(true);
                        tracing::debug!("[TestAgent] Injected error message: {text}");
                    } else {
                        label.set_visible(false);
                        label.set_text("");
                        tracing::debug!("[TestAgent] Cleared error message");
                    }
                }
                if let Some(warn_val) = messages.get("warning") {
                    // Use dedicated warning label if available, fall back to error label
                    let label_ref = current_warning_label.borrow();
                    let fallback_ref = current_error_label.borrow();
                    let label_opt = label_ref.as_ref().or(fallback_ref.as_ref());
                    if let Some(label) = label_opt {
                        if let Some(text) = warn_val.as_str() {
                            label.set_text(text);
                            label.set_visible(true);
                            if let Ok(mut s) = shared.lock() {
                                s.warning_text = Some(text.to_string());
                            }
                            tracing::debug!("[TestAgent] Injected warning message: {text}");
                        } else {
                            label.set_visible(false);
                            label.set_text("");
                            if let Ok(mut s) = shared.lock() {
                                s.warning_text = None;
                            }
                            tracing::debug!("[TestAgent] Cleared warning message");
                        }
                    }
                }
                if let Some(info_val) = messages.get("info") {
                    // Use dedicated info label if available, fall back to error label
                    let label_ref = current_info_label.borrow();
                    let fallback_ref = current_error_label.borrow();
                    let label_opt = label_ref.as_ref().or(fallback_ref.as_ref());
                    if let Some(label) = label_opt {
                        if let Some(text) = info_val.as_str() {
                            label.set_text(text);
                            label.set_visible(true);
                            if let Ok(mut s) = shared.lock() {
                                s.info_text = Some(text.to_string());
                            }
                            tracing::debug!("[TestAgent] Injected info message: {text}");
                        } else {
                            label.set_visible(false);
                            label.set_text("");
                            if let Ok(mut s) = shared.lock() {
                                s.info_text = None;
                            }
                            tracing::debug!("[TestAgent] Cleared info message");
                        }
                    }
                }
            }

            // --- Compose patch (file attachment bypass) ---
            if let Some(compose) = state.get("compose")
                && let Some(file_path) = compose.get("file").and_then(|v| v.as_str())
            {
                let target = compose.get("target").and_then(|v| v.as_str());
                if target == Some("attachment-button") {
                    // Conversations attach (`attachment-button`, wired
                    // `views/conversations/detail.rs`): unlike feed's
                    // deferred `stage_attachment`, `add_attachment`/
                    // `add_new_thread_attachment` need real bytes right
                    // away, so read the file now — standing in for the
                    // GTK file-chooser callback the same way the
                    // feed-only branch below stands in for
                    // `post_list.rs`'s (no OS file-chooser dialog in the
                    // e2e harness).
                    match std::fs::read(file_path) {
                        Ok(bytes) => {
                            let filename = std::path::Path::new(file_path)
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| "file".into());
                            let mime = fauna_conversations::compose::guess_mime_type(&filename)
                                .to_string();
                            let m = crate::conversations::manager();
                            let snap = m.snapshot();
                            // Same precedence `detail.rs::render` uses to
                            // pick which composer is showing.
                            if snap.new_thread_compose.is_some() {
                                m.add_new_thread_attachment(filename, mime, bytes);
                            } else if let Some(tid) = snap.selected_thread_id {
                                m.add_attachment(tid, filename, mime, bytes);
                            } else {
                                tracing::debug!(
                                    "[TestAgent] WARNING: conversations attach patch but no composer is active"
                                );
                            }
                        }
                        Err(e) => tracing::debug!(
                            "[TestAgent] WARNING: conversations attach patch failed to read {file_path}: {e}"
                        ),
                    }
                } else if target == Some("profile-edit-avatar")
                    || target == Some("profile-edit-banner")
                {
                    // Profile edit form avatar/banner picker
                    // (`views/profile/edit.rs`): no OS file-chooser dialog
                    // exists in the e2e harness (same reason the two
                    // branches above need a bypass), so stage the picked
                    // path directly onto the button widget — its own
                    // label IS the staged value `avatar_path_text()`/
                    // `banner_path_text()` read back. Found via
                    // `automation::find` (not `current_client`): the
                    // staged state lives on the currently-open edit
                    // form's widgets, not the process-wide client.
                    if let Some(id) = target
                        && let Some(widget) = crate::automation::find::find(id)
                        && let Some(btn) = widget.downcast_ref::<gtk::Button>()
                    {
                        btn.set_label(file_path);
                    } else {
                        tracing::debug!(
                            "[TestAgent] WARNING: {target:?} patch but no matching widget found"
                        );
                    }
                } else if let Some(ref client) = *current_client.borrow() {
                    tracing::debug!("[TestAgent] Staging file via compose patch: {file_path}");
                    // Stage; the blob uploads at post time (no race), then
                    // the post body carries a structured MediaItem.
                    client.stage_attachment(file_path);
                } else {
                    tracing::debug!(
                        "[TestAgent] WARNING: compose.file patch but no client available"
                    );
                }
            }

            // --- Nav patch ---
            if let Some(nav) = state.get("nav")
                && let Some(view) = nav
                    .get("stack")
                    .and_then(|s| s.as_array())
                    .and_then(|a| a.first())
                    .and_then(|f| f.get("view"))
                    .and_then(|v| v.as_str())
            {
                // Whether the Feed page was ALREADY showing before this nav — the
                // one Feed nav the page's own re-pull (`views/feed/mod.rs`'s
                // `connect_map`) cannot see, since nothing gets mapped. Read
                // before the switch below changes the answer.
                let feed_already_showing = current_stack
                    .borrow()
                    .as_ref()
                    .and_then(|s| s.visible_child_name())
                    .is_some_and(|n| n == "feed");
                if let Some(ref stack) = *current_stack.borrow() {
                    let gtk_target = test_agent::canonical_to_gtk(view);

                    // `{"view":"profile","actor_id":<hex>}` asks for a
                    // SPECIFIC actor's profile — that is a page REBUILD, not
                    // a stack switch. `views::profile::build_profile_view`
                    // takes its target at construction (`is_self =
                    // target.is_none()`), so `set_visible_child_name` alone
                    // always showed the SELF page and
                    // `ProfileActions.navigate_to_actor` was a silent no-op
                    // — the dropped-command shape testing.md convention 11
                    // forbids. Route it through the very same `open_profile`
                    // closure the sidebar (`open_profile(None)`) and the
                    // contacts tap-through (`open_profile(Some(id))`) use, so
                    // the state protocol and the two real UI paths agree
                    // (priority #3) — it rebuilds the child, re-points the
                    // Tiers refresh slot and makes itself visible, so no
                    // `set_visible_child_name` is needed on this branch.
                    //
                    // A bare `{"view":"profile"}` keeps the plain switch: it
                    // asks only to SHOW the page, and rebuilding there would
                    // discard in-page state a test had already set up.
                    let requested_actor = if gtk_target == "profile" {
                        nav.get("stack")
                            .and_then(|s| s.as_array())
                            .and_then(|a| a.first())
                            .and_then(|f| f.get("actor_id"))
                            .and_then(|v| v.as_str())
                    } else {
                        None
                    };
                    if let Some(actor) = requested_actor {
                        let opener = current_open_profile.borrow().clone();
                        if let Some(open_profile) = opener {
                            let me = current_client.borrow().as_ref().and_then(|c| c.actor_id());
                            open_profile(test_agent::profile_nav_target(actor, me.as_deref()));
                        } else {
                            // Convention 11 again: a profile-by-actor nav we
                            // cannot honour fails LOUDLY. Silence here would
                            // read downstream as "the follow button never
                            // rendered" — a product bug that isn't one.
                            let msg = format!(
                                "[test-agent] nav to profile actor_id='{actor}' \
                                     but no main window profile opener is registered"
                            );
                            tracing::error!("{msg}");
                            if let Some(ref label) = *current_error_label.borrow() {
                                label.set_text(&msg);
                                label.set_visible(true);
                            }
                        }
                    } else {
                        stack.set_visible_child_name(gtk_target);
                    }

                    // Sidebar-swap shells (admin AND settings): a second
                    // nav-stack entry selects a sub-page by `id`. The shell's
                    // content child is a Box holding the inner sub-`gtk::Stack`
                    // — switch that inner Stack to reach a specific sub-page
                    // (e.g. admin DNS, or settings → "privacy"/"mail-aliases").
                    // Scanning the Box's direct children for the lone Stack
                    // avoids threading handles through every
                    // `start_test_agent_if_enabled` call site.
                    // `AdminActions.navigate_settings()` /
                    // `MailAliasesActions.navigate()` send this two-element nav.
                    if gtk_target == "admin" || gtk_target == "settings" {
                        // An explicit second nav-stack entry selects the sub-page
                        // by `id`; for a single-element legacy nav into a settings
                        // sub-page (e.g. {"view":"devices"} / {"view":"folders"})
                        // derive the sub-page from the view itself so it still
                        // lands on the roster / folder control plane.
                        let explicit_sub = nav
                            .get("stack")
                            .and_then(|s| s.as_array())
                            .and_then(|a| a.get(1))
                            .and_then(|f| f.get("id"))
                            .and_then(|v| v.as_str());
                        let derived_sub = if gtk_target == "settings" {
                            test_agent::settings_subpage_for_view(view)
                        } else {
                            // A bare `{"view":"admin"}` nav (no sub-page `id`)
                            // means "the admin shell", which every other app
                            // resolves to the Dashboard — macOS
                            // (`AdminPage.swift`: `case "dashboard", nil`) and
                            // Windows (`AdminNavigation.cs`: an unrecognized/absent
                            // id lands on Dashboard, "so an admin nav never
                            // resolves to 'nowhere'"). Linux used to leave the
                            // inner stack on whatever sub-page was already
                            // showing, so re-navigating to admin from, say, the
                            // DNS sub-page silently stayed on DNS — a per-app
                            // divergence (priority #1), and the one that made the
                            // nest-kill crash-recovery journey red here but green
                            // on macOS.
                            Some("dashboard")
                        };
                        if let Some(sub_id) = explicit_sub.or(derived_sub) {
                            // The walk itself is `app::navigate_shell_subpage`
                            // — shared with the post-succession closing act's
                            // own "land on Settings § Account", so the agent
                            // and production take ONE door.
                            crate::app::navigate_shell_subpage(stack, gtk_target, sub_id);
                            // The p2p sub-page is built once (settings_shell.rs) and
                            // only self-refreshes after an in-page mutation (Accept
                            // Invite success, a removal) — re-navigating to it never
                            // rebuilds it, so a contact added while off-page (or by
                            // the e2e `p2p_seed_contact_for_test` fixture command)
                            // would otherwise render stale. Mirrors the "contacts"
                            // page's fetch-on-nav above.
                            if sub_id == "p2p" {
                                crate::settings::p2p_tab::refresh_contacts_list();
                            }
                            // The status sub-page's feature-limits section is
                            // fetched once at post-auth setup (app.rs) and never
                            // again — an admin policy change while the user is
                            // elsewhere in the app would otherwise render stale
                            // until the next full relaunch. Re-fetch on every
                            // re-nav (test_an_admin_limit_reaches_
                            // the_screen_naming_the_admin's "the nav edge re-reads"
                            // contract).
                            if sub_id == "status"
                                && let Some(ref client) = *current_client.borrow()
                            {
                                client.fetch_features();
                            }
                        }
                    }
                }
                // Refresh data for the target page so API-created
                // resources appear immediately after navigation.
                if let Some(ref client) = *current_client.borrow() {
                    match view {
                        // The Feed page re-pulls ITSELF when it becomes
                        // visible (`views/feed/mod.rs`'s `connect_map`: re-list
                        // feeds + `refresh_current_feed`), so a nav that shows
                        // it needs nothing here — a second re-pull on top made
                        // every entry reload twice, the second superseding the
                        // first. Only a nav to the page ALREADY showing (the
                        // re-selected tab, which maps nothing) re-pulls here,
                        // through the same shared seam (Trending preserved).
                        "feed" => {
                            if feed_already_showing && let Some(m) = crate::feed::host::manager() {
                                let rt = client.runtime_handle();
                                rt.spawn(async move {
                                    m.refresh_feeds().await;
                                    m.refresh_bridge_feeds().await;
                                    m.refresh_available_bridges().await;
                                    m.refresh_current_feed().await;
                                });
                            }
                        }
                        // The real sidebar's own door (`app.rs`), not a copy.
                        "contacts" => client.refresh_contacts_page(),
                        "events" => client.fetch_calendars(),
                        // The Devices/Peers page refreshes off the shared
                        // `DevicesMachine` when it becomes visible (wired on
                        // the stack in app.rs), so no per-nav fetch here.
                        // Shared with the real sidebar nav edge (`app.rs`'s
                        // content-stack notify) — see `FaunaClient::
                        // refresh_admin_shell`'s doc comment for what it
                        // covers and why. Kept here too (not just there)
                        // because this arm also covers the same-page
                        // `reload()` re-entry that a change-only notify
                        // can't see (the e2e agent can re-request "admin"
                        // while already on it).
                        "admin" => client.refresh_admin_shell(),
                        // The family page owns its own read (one
                        // `fauna.family.status` + `approvals.list` round trip),
                        // refreshed here on every nav patch — `connect_map`
                        // alone would miss a re-entry into the page it's
                        // already on (the e2e `reload()`).
                        "family" => crate::views::family::refresh_page(),
                        _ => {}
                    }
                }
                // Clear injected messages on navigation (mirrors real app
                // behavior where navigating away dismisses transient messages).
                for lbl_cell in [
                    current_error_label,
                    current_warning_label,
                    current_info_label,
                ] {
                    if let Some(ref label) = *lbl_cell.borrow() {
                        label.set_visible(false);
                        label.set_text("");
                    }
                }
                if let Ok(mut s) = shared.lock() {
                    s.warning_text = None;
                    s.info_text = None;
                }
            }
        }
        "call_machine_method" => {
            // E2E bridge: forward (method, json_arg) to the shared
            // `OnboardingMachine::call_machine_method` dispatcher per
            // the shared onboarding client target-state design's
            // §"E2E bridge contract" (tracked internally) — same path iOS, macOS, windows, android
            // and cli use, so all rust-dispatched test setters
            // (`set_*_for_test`, `set_step_for_test`, `seed_identity`, the
            // `navigate_to_*` helpers, …) work on Linux with no per-method
            // churn here. The earlier client-side match was a stopgap from
            // when `call_machine_method` was briefly reverted; it has since
            // been restored.
            //
            // The fields are top-level on the command (`{"action":
            // "call_machine_method", "method": …, "json_arg": …}`) — the one
            // wire shape `HttpBridgeDriver.call_machine_method` sends. Linux
            // formerly took them nested under a `"machine"` action's `state`;
            // it was the fleet's only sender of that shape.
            let name = cmd
                .payload
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let json_arg = match cmd.payload.get("json_arg") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => serde_json::to_string(other).unwrap_or_default(),
                None => String::new(),
            };
            // Machine-free bridge methods (the nest-identity pin seed/read) touch
            // only process-global state and need no machine, so they MUST work
            // post-auth, where no onboarding machine is registered
            // (`security.md` § Post-auth surfacing — the post-auth
            // identity-changed e2e re-seeds the pin on the LIVE session). Try the
            // shared free dispatcher first; only a `NeedsMachine` result falls
            // through to the machine path below.
            // The **registry** arms first (`refuse_secret_writes_for_test`,
            // `set_account_reach_for_test`, …). They need this app's own
            // `AccountRegistry`, which no free dispatcher can reach, so shared
            // Rust holds the name table and the semantics and linux hands over
            // only the registry — tui's delegation, one arm. The fault is a
            // reserved row in the store's backing, so the fresh store object
            // `account_registry()` builds here reaches the ceremony's registry
            // too. A registry hit reads as the free dispatcher's `Handled`.
            // (This whole handler is compiled out of a release build without
            // `e2e-agent`, as the shared fn is — convention 15.)
            let registry = fauna_client_accounts::call_registry_method_for_test(
                &crate::account_registry(),
                name,
                &json_arg,
            );
            let free = match registry {
                fauna_client_accounts::RegistryMethodOutcome::Handled(v) => {
                    fauna_onboarding_machine::FreeMethodOutcome::Handled(v)
                }
                fauna_client_accounts::RegistryMethodOutcome::NotMine => {
                    fauna_onboarding_machine::call_machine_free_method(name, &json_arg)
                }
            };
            let result = match free {
                fauna_onboarding_machine::FreeMethodOutcome::Handled(v) => v,
                fauna_onboarding_machine::FreeMethodOutcome::NeedsMachine => {
                    let Some(m) = current_onboarding_machine.borrow().clone() else {
                        // Convention 11: a test agent MUST NOT silently drop a
                        // command. A machine-requiring bridge method with no
                        // machine present is a genuine "cannot honour" — surface
                        // it LOUDLY on the app's `error-message`, never a debug
                        // log + bare return (the old shape here, which read
                        // downstream as a real product bug).
                        let msg = format!(
                            "[test-agent] call_machine_method '{name}' needs an onboarding \
                             machine, none is registered (post-auth?)"
                        );
                        tracing::error!("{msg}");
                        if let Some(ref label) = *current_error_label.borrow() {
                            label.set_text(&msg);
                            label.set_visible(true);
                        }
                        return;
                    };
                    if name == "start_provisioning" {
                        // The one arm the shared async dispatcher deliberately
                        // leaves to the client, because runtime *ownership* is
                        // platform-divergent: fire-and-forget, driving the real
                        // orchestrator to completion on a worker runtime — the
                        // exact mechanism the `provisioning-start-button` uses
                        // (`views/onboarding/nest_provisioning.rs`). Unlike
                        // `verify_dns` we do NOT block this handler: provisioning
                        // takes minutes (VPS create + ACME + boot), so the bridge
                        // returns immediately and the driver polls
                        // `provisioning_snapshot` to completion. `block_on_tokio`
                        // would be wrong here twice over — it would block for
                        // minutes, and its current-thread runtime is dropped on
                        // return, which would cancel anything merely *spawned*
                        // inside. The live Hetzner e2e
                        // (`tests/e2e-unified/tests/live/`) drives this; cancel
                        // still works via the shared `provisioning_cancel` flag.
                        // Reader value is None (it's a command).
                        let m = m.clone();
                        crate::async_helper::run_on_tokio(
                            async move { m.run_provisioning().await },
                            |_| {},
                        );
                        None
                    } else {
                        // Everything else — sync setters, readers
                        // (`provisioning_snapshot`, `provider_base_url`), the
                        // `set_*_for_test` fixtures, AND the async methods
                        // (`verify_dns`, `verify_vps`, `wizard_submit_claim_code`,
                        // …) — goes through the shared **async** dispatcher, which
                        // runs an async method to completion so the ack below
                        // fires only once its effect is in the snapshot
                        // (`onboarding.md` § E2E bridge contract; web parity: its
                        // `__fauna_callMachineMethod` awaits the promise). We block
                        // the GTK thread for that one local round-trip, as before —
                        // what changed is that the *name table* lives in shared
                        // Rust instead of being hand-listed here, so a newly-async
                        // machine method can no longer silently no-op on a client
                        // that forgot to add an arm.
                        let m = m.clone();
                        let name = name.to_string();
                        crate::async_helper::block_on_tokio(async move {
                            m.call_machine_method_async(name, json_arg).await
                        })
                    }
                }
            };
            // Stash the result (clearing any prior reader's value) before the
            // ack so the driver sees the result for *this* command.
            if let Ok(mut s) = shared.lock() {
                s.machine_method_result = result;
            }
        }
        // e2e-only: linux twin of tui's `device_set_state` automation arm
        // (`apps/fauna-tui/src/automation.rs`) — whether
        // `device_id_hex`'s plane `fauna.state.device-set` row reads
        // Removed/Enrolled from THIS app's own account runtime. A pure
        // on-demand reader, not a per-tick state key: it does async
        // local-store I/O, which convention 11's corollary forbids on the
        // state path, so it rides the same `machine_method_result`
        // reader-value wire contract `call_machine_method` uses — block the
        // GTK thread for one local read, exactly as `call_machine_method`'s
        // async arm above does.
        "device_set_state" => {
            let device_id_hex = cmd
                .payload
                .get("device_id_hex")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let handle = crate::account_runtime::handle();
            let value = crate::async_helper::block_on_tokio(async move {
                fauna_client_account_runtime::device_set_state_json(handle.as_ref(), &device_id_hex)
                    .await
            });
            if let Ok(mut s) = shared.lock() {
                s.machine_method_result = Some(value.to_string());
            }
        }
        // Force the held session bearer's refresh NOW through the production
        // `LaunchMachine::refresh_token` — linux's session bearer is the launch
        // machine's (`LaunchMachineBearer`), exactly as on tui — awaited so the
        // ack lands after the outcome: the wrong-clock refresh witness's ceremony
        // leg (`fauna_e2e_agent::LAUNCH_REFRESH_TOKEN`, case M).
        fauna_e2e_agent::LAUNCH_REFRESH_TOKEN => match settings::get_client() {
            Some(c) => {
                let machine = c.launch_machine();
                crate::async_helper::block_on_tokio(async move { machine.refresh_token().await });
            }
            // Convention 11: never silently dropped.
            None => report_refused_agent_command(
                shared,
                "launch_refresh_token",
                "no live authenticated client, so no launch machine to refresh",
            ),
        },
        // Trigger the production background silent challenge on the LIVE
        // authenticated session — the same `FaunaClient::silent_sign_in` call
        // `launch_authenticated` fires once at login, exposed to the bridge so the
        // post-auth identity-changed e2e can run it AFTER re-seeding a bogus pin
        // (`security.md` § Post-auth surfacing; `test_nest_identity_pin_post_auth.py`).
        // This drives the real path — `silent_sign_in` → `classify_silent_challenge`
        // → `DataMessage::NestIdentityChanged` → the launch surface — NOT a
        // shortcut that fakes the verdict.
        "silent_sign_in" => match settings::get_client() {
            Some(c) => c.silent_sign_in(),
            None => {
                // Convention 11: honour or fail loudly on `error-message`,
                // never silently drop.
                let msg = "[test-agent] silent_sign_in with no live authenticated client";
                tracing::error!("{msg}");
                if let Some(ref label) = *current_error_label.borrow() {
                    label.set_text(msg);
                    label.set_visible(true);
                }
            }
        },
        // Unified conversations page — three e2e bridge commands that
        // mirror ConversationsCommands.cs on Windows. Both paths share
        // the same JSON shape so the cross-app action layer at
        // tests/e2e-unified/actions/conversations.py works unchanged.
        "conversations_inject_inbound" => {
            handle_conversations_inject_inbound(cmd);
        }
        // Drop a thread's cached attachment bytes the way the store's budget
        // eviction does (the shared `evict_thread_attachments_for_test`), so a test
        // reaches the re-fetch of an evicted attachment and the declared placeholder
        // of one with nowhere to be fetched from, without filling the 128 MiB store.
        "conversations_evict_attachment" => {
            handle_conversations_evict_attachment(cmd, shared);
        }
        // Seed a pre-resolved link-preview (render-model.md § D4) so an injected
        // bubble's `LinkPreview` block folds `Resolved` and the bubble paints the
        // `link-preview-card` — the conversations twin of the feed's `link_preview`
        // inject spec. The resolve is mocked (tier_2); a real one needs a live nest
        // OpenGraph fetch.
        "conversations_seed_resolved_link_preview" => {
            handle_conversations_seed_resolved_link_preview(cmd);
        }
        "conversations_create_mls_group" => {
            handle_conversations_create_mls_group(cmd);
        }
        "conversations_accept_recipient" => {
            handle_conversations_accept_recipient(cmd, shared);
        }
        // Deterministic compose-send-failure injection for the page-level
        // `error-message` surface (conversations.md § Errors). A mail-OFF nest
        // enqueues + returns Ok, so no product path fails a send on demand —
        // this stamps the same `send_state = Failed { reason }` a real backend
        // error leaves, on the selected thread, so the e2e can assert it renders.
        "conversations_inject_send_failure" => {
            handle_conversations_inject_send_failure(cmd);
        }
        // Drive `select_thread_and_message` directly — the same call a Search
        // `Mail` result makes (`views/search.rs`), without needing a local
        // index segment to search. Apple's twin: `ConversationsSendTestCommand
        // .selectMessage`. `{thread_id, message_id}`.
        "conversations_select_message" => {
            handle_conversations_select_message(cmd, shared);
        }
        // Deterministic membership/label-failure injection for the same
        // page-level `error-message` surface's OTHER producer
        // (`ConversationsSnapshot.error` — confirm_add_participant /
        // remove_participant / rename_thread). No product path fails one of
        // those ops on demand, so this stamps the same observable state a
        // real failure leaves.
        "conversations_inject_page_error" => {
            handle_conversations_inject_page_error(cmd);
        }
        // Real-wire FaunaMls opt-in + drivers (tier_3
        // test_fauna_mls_real_roundtrip.py).
        // These opt the linux app into the REAL FaunaMls backend and drive
        // its async manager wire-drivers with the API-tier peer's actor_id
        // injected (the resolve_address handle→actor chain stays deferred).
        "conversations_enable_real_faunamls" => {
            crate::conversations::conv_backend::request_e2e_activation();
        }
        "conversations_disable_real_faunamls" => {
            crate::conversations::conv_backend::disable_e2e_real_backend();
        }
        "conversations_real_resolve_send_new" => {
            handle_conversations_real_resolve_send_new(cmd, shared);
        }
        "conversations_real_send" => {
            handle_conversations_real_send(cmd, shared);
        }
        "conversations_real_send_attachment" => {
            handle_conversations_real_send_attachment(cmd, shared);
        }
        "conversations_real_add" => {
            handle_conversations_real_add(cmd, shared);
        }
        "conversations_real_remove" => {
            handle_conversations_real_remove(cmd, shared);
        }
        "conversations_real_rename" => {
            handle_conversations_real_rename(cmd, shared);
        }
        // Screen time (family-safety.md § Screen time, Slice E) — advance the
        // ward's heartbeat clock by `minutes` of foreground use and flush a
        // report, so a tier_3 journey can prove the budget half WITHOUT waiting
        // on wall-clock time. This is convention 14's fake clock + `run_now`
        // poke: a test that slept for a real heartbeat interval would be
        // *defunct* under testing.md § point 14, not merely slow. The cadence
        // and accrual rules themselves are pure and already proven at tier_1
        // (`fauna_core::screen_time::tests`); what this exercises is the WIRING
        // — that the client really calls `fauna.family.usage_report` and feeds
        // the reply back into the lock. Compiled out of release artifacts
        // (convention 15), like every other command in this table.
        "screen_time_heartbeat" => {
            let minutes: i64 = cmd
                .payload
                .get("minutes")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            crate::screen_lock::advance_test_clock(minutes * 60);
            if let Some(c) = current_client.borrow().as_ref() {
                // `focused = true`: the poke asserts what a ward actively using
                // the app accrues. Real focus is a window-manager fact that a
                // headless Xvfb run cannot be trusted to report.
                c.flush_usage_report(true);
            }
        }
        // Feed page — inject a snapshot post list straight onto the shared
        // `FeedManager` so the page paints posts with an arbitrary `verification`,
        // the only way to exercise the `Failed` unverified-source-badge render (a
        // real signed post is only ever `Unchecked`/`Verified`; `security.md`
        // § App display of unverified content). Browser twin: web's
        // `feed_inject_posts` `__fauna_callCommand` case. tier_2
        // `tests/e2e-unified/tests/test_feed_unverified_source.py`.
        "feed_inject_posts" => {
            handle_feed_inject_posts(cmd);
        }
        // Seed the live engagement-cue engine with a real `cues:v1` nest row (a
        // real network round trip, unlike `feed_inject_posts` above), so a
        // capture-less test can reach "Clear activity data" with something to
        // actually delete. Twin of tui's arm and windows' `SetCueRollupForTest`.
        // `{content_ids: string[]}`. See `fauna_feed::FeedManager::set_cue_rollup_for_test`.
        "feed_seed_cue_rollup_for_test" => {
            handle_feed_seed_cue_rollup_for_test(cmd, shared);
        }
        // Arm / release the feed manager's one-shot reload hold
        // (`FeedManager::hold_next_reload_for_test`): the NEXT reload publishes
        // the list it kept or cleared, then parks before its fetch until the
        // release, so a test can read the page while a refresh is in flight
        // (`feed.md` § The read model). The feed's gestures spawn their reload
        // on the GTK main context, never block the agent on it, so nothing here
        // has to start-rather-than-await while a hold is armed (tui's
        // `feed_reload_held`). Twins of tui's arms of the same names.
        // Stamp `FeedSnapshot.error` — the state a failed background fetch
        // leaves — so the page paints it on `error-message` (no product path
        // fails a feed fetch on demand). `{key?, message?}`, the one
        // `LocalizedText::key_arg` carrier a real `feed.error_load` uses. Twin
        // of tui's and web's `feed_inject_error`.
        "feed_inject_error" => {
            let Some(manager) = crate::feed::host::manager() else {
                report_agent_command_failure(shared, &cmd.action, "no feed manager (pre-auth)");
                return;
            };
            let arg = |k: &str, default: &str| {
                cmd.payload
                    .get(k)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(default)
                    .to_string()
            };
            manager.inject_error_for_test(fauna_core::localized::LocalizedText::key_arg(
                arg("key", "feed.error_load"),
                "message",
                arg("message", "feed load failed"),
            ));
        }
        "feed_hold_next_reload" | "feed_release_held_reload" => {
            let Some(manager) = crate::feed::host::manager() else {
                report_agent_command_failure(shared, &cmd.action, "no feed manager (pre-auth)");
                return;
            };
            if cmd.action == "feed_hold_next_reload" {
                manager.hold_next_reload_for_test();
            } else {
                manager.release_held_reload_for_test();
            }
        }
        // ATProto settings — move the D10 delegation row's RENDER clock (never
        // the mint clock; a mint always uses the real wall clock) so the lapse
        // e2e can reach `expiring_soon` / `expired` without sleeping out the
        // real ~90-day window (convention 14 — a fake clock, never a sleep).
        // `now_offset_secs: 0` resets it; the offset is process-wide and
        // nothing auto-resets it (`delegation_clock` module docs). tui's twin
        // is `automation.rs`'s arm of the same name; apple's is
        // `DelegationClockTestCommand.swift`.
        //
        // The nudge is what makes the new offset visible: the page repaints off
        // the machine's snapshot, so the offset alone changes nothing until the
        // machine re-`refresh()`es. `notify_atproto_rehydrate` is the hook the
        // page registers for exactly that (it is also what the
        // `fauna.atproto.consent_requested` push fires); the e2e then polls the
        // `state` attr for the new liveness rather than assuming this acked
        // after the repaint.
        "atproto_delegation_advance_clock" => {
            let offset = cmd
                .payload
                .get("now_offset_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            fauna_atproto_settings_machine::set_delegation_clock_offset_secs(offset);
            crate::settings::notify_atproto_rehydrate();
        }
        // Moves the Nests trust facet's RENDER clock
        // (`fauna_client_capabilities::trust_clock`) — grant liveness, the
        // auto-renew due decision and custody receipt freshness, never the
        // mint clock — so a lapse journey reaches `expiring soon` / `paused`
        // by convention 14's fake clock. tui's twin is `automation.rs`'s arm.
        // `now_offset_secs: 0` resets; the offset is process-wide. Nothing to
        // repaint: the Nests page folds against it on its nav-edge hydrate,
        // and the driver's wait re-navigates on every poll.
        "trust_facet_advance_clock" => {
            let offset = cmd
                .payload
                .get("now_offset_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            fauna_client_capabilities::trust_clock::set_clock_offset_secs(offset);
        }
        // Moves the co-present ceremony's ADMISSION clock
        // (`fauna_sync_engine::ceremony_clock`), the `now` a receive-act
        // expectation is minted and judged against — outcome 6's lapsed
        // receive window, reached by convention 14's fake clock, never a
        // sleep. tui's twin is `automation.rs`'s arm of the same name, which
        // carries the full rationale. `now_offset_secs: 0` resets; the offset
        // is process-wide. Nothing to repaint: the seat's listener reads it at
        // admission time.
        #[cfg(feature = "p2p-share")]
        "offline_share_advance_clock" => {
            let offset = cmd
                .payload
                .get("now_offset_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            fauna_sync_engine::ceremony_clock::set_clock_offset_secs(offset);
        }
        // Drops every connection a counterpart has open to this session's
        // ceremony listener, keeping it up — outcome 5's link failing
        // part-way. Returns how many were dropped as the machine result, so
        // the journey can prove there was one; with no seat bound it fails
        // loudly (convention 11). tui's twin is `automation.rs`'s arm.
        #[cfg(feature = "p2p-share")]
        "offline_share_drop_connections" => {
            let seat = crate::offline_share::session_seat().and_then(|s| s.seat());
            let dropped = seat.map(|seat| seat.node.close_inbound());
            if let Ok(mut s) = shared.lock() {
                s.machine_method_result = dropped.map(|n| n.to_string());
            }
            if dropped.is_none() {
                report_agent_command_failure(shared, &cmd.action, "no ceremony seat is bound");
            }
        }
        // Turns the share plane's SERVE hold on or off
        // (`fauna_sync_engine::share_serve_tally`) — the "part-way through a
        // transfer" state outcome 8's resume journey waits for, cuts, and
        // releases. Process-wide; the journey lifts it before asserting
        // arrival. tui's twin carries the full rationale.
        #[cfg(feature = "p2p-share")]
        "offline_share_hold_serves" => {
            let on = cmd
                .payload
                .get("hold")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            fauna_sync_engine::share_serve_tally::set_hold(on);
        }
        // Reads one shared set straight off another seat's share plane as
        // THIS seat's identity and reports what came back — outcome 7's
        // stranger probe and its member control. The parse and the probe are
        // one shared call (`share_probe::probe_set_from_args`), the body tui's
        // arm runs too. Blocks the GTK thread for the probe's round trips, the
        // `device_set_state` shape above; fails loudly with no seat bound or a
        // refused argument (convention 11).
        #[cfg(feature = "p2p-share")]
        "offline_share_probe_set" => {
            let seat = crate::offline_share::session_seat().and_then(|s| s.seat());
            let peer_code = cmd_str(cmd, "peer_code").to_string();
            let group_id_hex = cmd_str(cmd, "group_id_hex").to_string();
            let hashes: Vec<String> = cmd
                .payload
                .get("manifest_hashes")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|h| h.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let outcome = match seat {
                Some(seat) => crate::async_helper::block_on_tokio(async move {
                    fauna_sync_engine::share_probe::probe_set_from_args(
                        &seat.node,
                        &peer_code,
                        &group_id_hex,
                        &hashes,
                    )
                    .await
                }),
                None => Err("no ceremony seat is bound".to_string()),
            };
            let result = outcome
                .as_ref()
                .ok()
                .and_then(|r| serde_json::to_string(r).ok());
            if let Ok(mut s) = shared.lock() {
                s.machine_method_result = result;
            }
            if let Err(e) = outcome {
                report_agent_command_failure(shared, &cmd.action, &e);
            }
        }
        // P2P page — seed a contact directly on the local PeerDb; the only
        // write path since the Accept-Invite dialog was retired (p2p.md § No
        // pairing step, ever). Fixture
        // setup, not the mutation under test: `test_p2p.py`'s removal test
        // mutates through the real `p2p-contact-remove-button` click
        // (e2e-conventions point 8) — this command only arranges the
        // precondition of "a contact already exists".
        "p2p_seed_contact_for_test" => {
            handle_p2p_seed_contact_for_test(cmd);
        }
        // P2P page — force the NEXT `p2p-tunnel-toggle` Start click to hit a
        // REAL bind conflict (e2e-conventions point 8's carve-out (b): a
        // held-open loopback socket, not a simulated error), so
        // `test_p2p.py`'s error-message test drives the actual failure path
        // through the real button rather than asserting by inspection.
        "p2p_force_bind_conflict_for_test" => {
            if let Some(p2p) = crate::settings::get_p2p() {
                if let Err(e) = p2p.force_bind_conflict_for_test() {
                    tracing::debug!("[TestAgent] p2p_force_bind_conflict_for_test: {e}");
                }
            } else {
                tracing::debug!(
                    "[TestAgent] p2p_force_bind_conflict_for_test: no P2pService (pre-auth)"
                );
            }
        }
        // Release the fixture above so a later test's Start click binds normally.
        "p2p_clear_bind_conflict_for_test" => {
            if let Some(p2p) = crate::settings::get_p2p() {
                p2p.clear_bind_conflict_for_test();
            }
        }
        // Live location-map edits (tests/e2e-unified — Sync-tab live-apply). These
        // mirror the Settings → Folders add/remove handlers but bypass the
        // native GTK folder picker (`gtk::FileDialog`): the binding routes to
        // the external `fauna-sync-agent` over the per-user socket (optimistic
        // model + union reconcile — `crate::sync_agent`), exactly like a user
        // click; under e2e the agent is the real binary, direct-spawned into
        // this launch's isolated XDG world.
        "sync_add_location" => {
            let path = cmd_str(cmd, "path").to_string();
            let folder = cmd_str(cmd, "folder").to_string();
            // The set's `FolderRef` wire form — the bind is keyed by it alone,
            // exactly as the Folders UI's gesture is (`folder_ref_for_row`).
            let folder_id = cmd_str(cmd, "folder_id").to_string();
            if path.is_empty() || folder.is_empty() || folder_id.is_empty() {
                // Convention 11: honour it or fail loudly — never a silent no-op.
                report_agent_command_failure(
                    shared,
                    &cmd.action,
                    "needs `path`, `folder` and `folder_id`",
                );
            } else {
                crate::sync_agent::remove_binding(&folder);
                crate::sync_agent::add_binding(crate::sync::LocationBinding {
                    path: std::path::PathBuf::from(&path),
                    folder,
                    folder_id,
                });
            }
        }
        "sync_remove_location" => {
            let folder = cmd_str(cmd, "folder").to_string();
            if !folder.is_empty() {
                crate::sync_agent::remove_binding(&folder);
            }
        }
        // Seed the Settings → Sync bound-folder list for the cross-app
        // Sync-folders e2e (tests/e2e-unified/tests/test_sync_folders.py, tier_2).
        // Replaces the live list with the injected mappings (the GTK peer of the
        // windows `InMemorySyncPipeClient` swap) so the page's render/dispatch
        // surface can be driven without the native folder picker (`gtk::FileDialog`). Unlike
        // `sync_add_location` this touches the *live UI* (not the persisted map)
        // and starts no engine — it's a pure render fixture.
        "sync_inject_locations" => {
            handle_sync_inject_locations(cmd);
        }
        "rpc_echo" => {
            // WS-RPC round-trip probe used by
            // tests/e2e-unified/tests/test_sp_linux_ws_rpc_echo.py.
            // Reads `data` (UTF-8 string), sends it as `fauna.protocol.echo`
            // bytes via the long-lived `NestClient`, writes the hex-encoded
            // reply into `shared.rpc_echo_reply`. Clears any prior reply
            // so the Python driver can detect this run's completion.
            let data = cmd
                .payload
                .get("data")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .as_bytes()
                .to_vec();
            if let Ok(mut s) = shared.lock() {
                s.rpc_echo_reply = None;
            }
            if let Some(client) = current_client.borrow().as_ref() {
                client.rpc_echo(data, shared.clone());
            } else if let Ok(mut s) = shared.lock() {
                s.rpc_echo_reply = Some(test_agent::RpcEchoOutcome::Err {
                    error: "fauna client not initialized".to_string(),
                });
            }
        }
        "enable_caldav_mailbox" => {
            // Slice C (test_caldav_autoschedule_mailbox_less.py): mint the
            // currently logged-in actor's shared MSEK via the CalDAV-enable recipe
            // so a mailbox-less GUI attendee's `NestSchedulingSink` can materialize
            // a server-side auto-schedule invite. Writes the outcome into
            // `shared.caldav_mailbox_reply`; clears any prior reply so the driver
            // detects this run's completion (the same shape as `rpc_echo`).
            if let Ok(mut s) = shared.lock() {
                s.caldav_mailbox_reply = None;
            }
            // Optional `password` → mint the credential with a known secret (so a
            // stock CalDAV client can AUTH as this actor); absent → generated.
            let password = cmd
                .payload
                .get("password")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if let Some(client) = current_client.borrow().as_ref() {
                client.enable_caldav_mailbox_for_test(password, shared.clone());
            } else if let Ok(mut s) = shared.lock() {
                s.caldav_mailbox_reply = Some(test_agent::CalDavMailboxOutcome::Err {
                    error: "fauna client not initialized".to_string(),
                });
            }
        }
        "serve_enable_folder" => {
            // WebDAV read+write tier_3 e2e
            // (tests/e2e-unified/tests/test_webdav_read_write_roundtrip.py): arrange
            // the served-set precondition for the currently logged-in actor —
            // (optionally create) an empty Sync set, run serve_enable + reconcile
            // the WebdavKeysBlob — so the WebDAV client can PUT/GET it. Writes the
            // outcome into `shared.webdav_serve_reply`; clears any prior reply so
            // the driver detects this run's completion (same shape as rpc_echo).
            if let Ok(mut s) = shared.lock() {
                s.webdav_serve_reply = None;
            }
            let set = cmd_str(cmd, "folder").to_string();
            // Default `create: true` — the e2e wants a fresh empty served set.
            let create = cmd
                .payload
                .get("create")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if set.is_empty() {
                if let Ok(mut s) = shared.lock() {
                    s.webdav_serve_reply = Some(test_agent::WebdavServeOutcome::Err {
                        error: "serve_enable_folder requires a non-empty folder".to_string(),
                    });
                }
            } else if let Some(client) = current_client.borrow().as_ref() {
                client.serve_enable_folder_for_test(set, create, shared.clone());
            } else if let Ok(mut s) = shared.lock() {
                s.webdav_serve_reply = Some(test_agent::WebdavServeOutcome::Err {
                    error: "fauna client not initialized".to_string(),
                });
            }
        }
        "backup_audit_run_now" => {
            // Backups-page audit-alert e2e (tests/e2e-unified/tests/test_backups.py).
            // Runs one **real** audit pass — real connection to each configured
            // destination, real `fauna.backup.custody.list`, the client's own
            // persisted observation high-water — with only the clock shifted by
            // `now_offset_secs`. That shift is what a staleness proof needs and
            // cannot fake any other way: freshness floors a destination's
            // high-water at its `added_at`, so a destination enrolled seconds
            // ago is *correctly* never stale in real time (convention 14 — poke
            // the clock, never sleep out a 48 h window).
            let offset = cmd
                .payload
                .get("now_offset_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            backup_audit::set_clock_offset_secs(offset);
            // Convention 11: a command the app cannot honour fails loudly on the
            // page's own `error-message`, never silently.
            if !backup_audit::poke_rerun()
                && let Some(label) = current_error_label.borrow().as_ref()
            {
                label.set_text("backup_audit_run_now: the Backups page is not built");
                label.set_visible(true);
            }
        }
        // Client-custodian e2e (tests/e2e-unified/tests/test_backups.py::
        // test_a_hosted_custodian_pulls_checks_in_and_the_owners_row_reports_it).
        // Runs ONE custodian pull pass on the sync agent's hosted replica and
        // returns what it did, as `state.machine_method_result`. The production
        // first pass is `PERIODIC_INTERVAL` away — `CustodianPull::run_loop`
        // deliberately mutes the interval's immediate first tick — so this is
        // the causal barrier the tier_3 enroll→pull→check-in→status proof runs
        // on, never a settle-sleep (convention 14). Blocks the GTK thread for
        // one local agent round trip, the same bridge the "machine" dispatch
        // above already uses to ack only once an async result is in hand — the
        // tui twin of this arm (`apps/fauna-tui/src/automation.rs`).
        "custodian_pull_run_now" => {
            // Convention 14's fake clock, same `now_offset_secs` spelling as
            // `backup_audit_run_now` above and tui's twin arm. Absent/0 is an
            // ordinary pass at the real clock; a positive offset is how a test
            // reaches the self-audit's 24-hour debounce without sleeping a day
            // out.
            let offset = cmd
                .payload
                .get("now_offset_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let provisioner = crate::sync_agent::provisioner_and_runtime().map(|(p, _rt)| p);
            match provisioner {
                Some(provisioner) => {
                    match crate::async_helper::block_on_tokio(async move {
                        provisioner.custodian_run_pass_now(offset).await
                    }) {
                        Ok(report) => {
                            if let Ok(mut s) = shared.lock() {
                                s.machine_method_result = Some(
                                    serde_json::json!({
                                        "hosting": report.hosting,
                                        "kinds_run": report.kinds_run,
                                        "held_bytes": report.held_bytes,
                                        "cap_state": report.cap_state,
                                        "audit_state": report.audit_state,
                                        "checked_in": report.checked_in,
                                    })
                                    .to_string(),
                                );
                            }
                        }
                        // Convention 11: a command the app cannot honour fails
                        // loudly on the page's own `error-message`, never
                        // silently. The result slot is rewritten on every
                        // command (never a stale neighbour's), so a refusal
                        // clears it rather than leaving a prior report behind.
                        Err(e) => {
                            if let Ok(mut s) = shared.lock() {
                                s.machine_method_result = None;
                            }
                            if let Some(label) = current_error_label.borrow().as_ref() {
                                label.set_text(&format!("custodian_pull_run_now: {e}"));
                                label.set_visible(true);
                            }
                        }
                    }
                }
                // No sync agent surface on this platform / not installed yet —
                // the same honest "no agent" refusal tui reports, distinct
                // from an agent that answered and refused the op.
                None => {
                    if let Ok(mut s) = shared.lock() {
                        s.machine_method_result = None;
                    }
                    if let Some(label) = current_error_label.borrow().as_ref() {
                        label.set_text(
                            "custodian_pull_run_now: linux drives no sync agent on this platform",
                        );
                        label.set_visible(true);
                    }
                }
            }
        }
        // Contract: `fauna_e2e_agent::ALERT_SWEEP_WAKE` — end the current
        // identity's re-sweep wait so the production loop sweeps again; the
        // caller's barrier is `alert_sweep_passes`, never this ack. The tui arm
        // is the reference.
        fauna_e2e_agent::ALERT_SWEEP_WAKE => {
            if !crate::critical_alerts::wake_sweep() {
                // Convention 11: no identity's loop is running, so the wake
                // would be acked and read by nobody.
                report_refused_agent_command(
                    shared,
                    fauna_e2e_agent::ALERT_SWEEP_WAKE,
                    "no authenticated session, so no sweep loop to wake",
                );
            }
        }
        // Contract: `fauna_e2e_agent::RECONNECT_BACKOFF` — pace this session's
        // reconnect retries (never the `Unreachable` threshold), or restore
        // them with `{}`. The tui arm is the reference.
        fauna_e2e_agent::RECONNECT_BACKOFF => {
            match (
                current_client.borrow().as_ref(),
                fauna_e2e_agent::reconnect_backoff_bounds(&cmd.payload),
            ) {
                (Some(client), Ok(bounds)) => {
                    client.nest_rpc().set_reconnect_backoff_for_test(bounds)
                }
                // Convention 11: a pace that did not land leaves the production
                // one in force, and the journey would spend its budget waiting.
                (None, _) => report_refused_agent_command(
                    shared,
                    fauna_e2e_agent::RECONNECT_BACKOFF,
                    "no fauna client, so no connection to pace",
                ),
                (Some(_), Err(reason)) => report_refused_agent_command(
                    shared,
                    fauna_e2e_agent::RECONNECT_BACKOFF,
                    &reason,
                ),
            }
        }
        fauna_e2e_agent::FAMILY_NOTIFY_CHECK_NOW => {
            // Guardian Notify flush cadence e2e command (convention 14's
            // "run_now poke"), the linux twin of web's
            // `family_notify_check_now` ($lib/family-notify-e2e.ts). Drives
            // `FaunaClient::flush_notify_report` directly rather than waiting
            // out the real 5s poll tick, which `start_notify_flush_poll` no
            // longer arms under e2e (`crate::e2e_mode_enabled()`) — so under
            // test this poke is the only thing that ever checks.
            if let Some(client) = current_client.borrow().as_ref() {
                client.flush_notify_report();
            } else if let Some(label) = current_error_label.borrow().as_ref() {
                label.set_text("family_notify_check_now: fauna client not initialized");
                label.set_visible(true);
            }
        }
        fauna_e2e_agent::CONV_RECEIVE_NOW => {
            // The receive loop's run-one-cycle-now poke (convention 14's
            // `run_now`), the linux twin of tui's arm and web's
            // `conv_receive_now` ($lib/conversations.ts). Signals the shared
            // loop, which runs the identical `full_sweep!` its 30 s backstop
            // ticker runs — the real delivery path, not a per-rail shortcut.
            // Fire-and-forget: the barrier is `conv_receive_cycles`, not this
            // ack. No session yet (pre-auth) is a legitimate quiet no-op — the
            // consumer's own deadline poll is what fails, naming the app.
            if let Some(session) = crate::conversations::conv_backend::active_session().as_ref() {
                session.poke_receive_cycle();
            } else {
                tracing::info!("conv_receive_now: no conversations session yet (pre-auth)");
            }
        }
        fauna_e2e_agent::ACCOUNT_PUMP_NOW => {
            // The account-plane pump's run-one-pass-now poke (convention 14's
            // `run_now`), the linux twin of tui's arm — `reconcile_now` is the
            // ticker's own work on demand, never a bypass.
            // `fauna_e2e_agent::ACCOUNT_PUMP_NOW` owns the contract.
            //
            // Fire-and-forget like the receive poke: the barrier is the
            // `account_pump_cycles` counters, not this ack. No store yet
            // (pre-auth, or an assembly that has not landed) is a legitimate
            // quiet no-op — the consumer's own deadline poll is what fails,
            // and it names the app.
            //
            // Like tui's arm, the pass chains one custody ceremony drive
            // (`devices.md` § Custody facet pieces 1 + 3): a receipt the pass
            // just minted, or a registry write it owed, posts without waiting
            // for a production edge.
            match (
                crate::account_runtime::handle(),
                current_client.borrow().as_ref(),
            ) {
                (Some(store), Some(client)) => {
                    let nest = std::sync::Arc::clone(client.nest_rpc());
                    let secret = client.secret_bytes();
                    let session = crate::conversations::conv_backend::active_session();
                    client.runtime_handle().spawn(async move {
                        match store.reconcile_now().await {
                            Ok(report) => {
                                tracing::info!(?report, "account_pump_now: pass complete")
                            }
                            Err(e) => tracing::warn!("account_pump_now: {e}"),
                        }
                        fauna_client_custody::spawn_drive(nest, secret, session, Some(store));
                    });
                }
                _ => tracing::info!("account_pump_now: no account store yet (pre-auth)"),
            }
        }
        // The barrier's self-test probe: queue UI work on the glib **idle**
        // queue — the same queue real deferred UI work rides — and let the
        // normal ack below fire without waiting for it. The early ack is the
        // point: only a correct `barrier` can make the token observable.
        fauna_e2e_agent::BARRIER_PROBE => {
            let token = cmd
                .payload
                .get("token")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if token.is_empty() {
                // Convention 11: a token-less probe would ack green and prove
                // nothing — the silent drop wearing a disguise.
                if let Some(label) = current_error_label.borrow().as_ref() {
                    label.set_text("barrier_probe: payload needs a non-empty `token`");
                    label.set_visible(true);
                }
            } else {
                let count = cmd
                    .payload
                    .get("count")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(fauna_e2e_agent::BARRIER_PROBE_DEFAULT_COUNT);
                for i in 0..count {
                    let value = fauna_e2e_agent::barrier_probe_value(&token, i);
                    glib::idle_add_local_once(move || {
                        automation::link::record_barrier_probe(&value);
                    });
                }
            }
        }
        // `barrier` itself is handled entirely at the ack site below — see the
        // `defer_ack` comment there for why linux cannot ack from this handler.
        fauna_e2e_agent::BARRIER => {}
        // Convention 17 layer (c)'s walk vocabulary. Both arms drive GTK's own
        // focus machinery through `child_focus`, which is the function GTK's
        // default Tab/Shift-Tab handler itself calls — so a walk exercises the
        // identical traversal a human's keystroke performs, as convention 17's
        // "walks drive the real input doors" requires. Reaching into the widget
        // tree to `grab_focus` a chosen widget would be the private seam that
        // rule exists to forbid: it can reach states the keyboard cannot, and it
        // would step straight over the focus order that is itself under test.
        fauna_e2e_agent::FOCUS_MOVE => match fauna_e2e_agent::focus_move_request(&cmd.payload) {
            Ok((direction, times)) => {
                let dir = match direction {
                    fauna_e2e_agent::FocusDirection::Next => gtk::DirectionType::TabForward,
                    fauna_e2e_agent::FocusDirection::Prev => gtk::DirectionType::TabBackward,
                };
                match application.active_window() {
                    Some(win) => {
                        for _ in 0..times {
                            // A false return means the ring had nowhere further
                            // to go in that direction. That is a legitimate
                            // answer at an edge, not a failure, and the walk's
                            // own invariants are what judge the resulting state.
                            let _ = win.child_focus(dir);
                        }
                    }
                    // Not a silent no-op: a walk stepping against a window that
                    // is not up would otherwise read every step as "focus did
                    // not move" and conclude the app has one focusable widget.
                    None => report_refused_agent_command(
                        shared,
                        fauna_e2e_agent::FOCUS_MOVE,
                        "no active window — the app is not mounted yet",
                    ),
                }
            }
            Err(reason) => {
                report_refused_agent_command(shared, fauna_e2e_agent::FOCUS_MOVE, &reason)
            }
        },
        fauna_e2e_agent::SWITCH_PANE => match fauna_e2e_agent::switch_pane_target(&cmd.payload) {
            Ok(pane) => match split_view_of(application) {
                Some(split) => {
                    let region = match pane {
                        fauna_e2e_agent::Pane::Page => split.content(),
                        fauna_e2e_agent::Pane::Sidebar => split.sidebar(),
                    };
                    match region {
                        // Enter the region through its own first tab stop —
                        // again GTK's real traversal, not a chosen widget.
                        Some(w) => {
                            let _ = w.child_focus(gtk::DirectionType::TabForward);
                        }
                        None => report_refused_agent_command(
                            shared,
                            fauna_e2e_agent::SWITCH_PANE,
                            "the split view has no widget in that slot",
                        ),
                    }
                }
                None => report_refused_agent_command(
                    shared,
                    fauna_e2e_agent::SWITCH_PANE,
                    "no OverlaySplitView — the authenticated shell is not mounted",
                ),
            },
            Err(reason) => {
                report_refused_agent_command(shared, fauna_e2e_agent::SWITCH_PANE, &reason)
            }
        },
        // ⚠ **NOT a `{}`.** Convention 11: an action with no arm is refused
        // loudly on the app's own error surface, never dropped. This arm read
        // `_ => {}` until 2026-08-16 and made linux the only silent agent in the
        // fleet — see `report_refused_agent_command` for the full account, and
        // `tests/test_agent_refuses_unknown_command.py` for the pin. A command
        // linux legitimately ignores gets its **own** named arm with a comment
        // saying why; it does not come back here.
        other => report_refused_agent_command(
            shared,
            other,
            "linux's test agent has no arm for this action",
        ),
    }

    // ⚠ **Mutation-grading status: PINNED.** Deleting this whole block (acking
    // from the drain below instead) reds
    // `test_agent_barrier.py::test_fused_barrier_probe_orders_the_batch --app
    // linux` — measured 2026-08-13, the M4 mutant that previously survived.
    // What changed is the *test*, not this mechanism: the fused `barrier_probe`
    // (`fauna_e2e_agent::BARRIER_PROBE_FUSE_FIELD`) enqueues its batch and
    // barriers inside ONE command, so the ~50 ms of otherwise-idle main loop
    // that used to sit between the two commands — and drained every queued idle
    // in that gap regardless of what this block did — no longer exists. The
    // older two-command test remains and still cannot discriminate this block;
    // it is kept as the usage-shape smoke, and says so.
    //
    // ⚠ `barrier` MUST NOT ack from here (`e2e-conventions.md` § convention 14).
    // This handler runs inside the 50 ms `glib::timeout_add_local` command drain,
    // and glib runs **timeout** sources (priority DEFAULT = 0) ahead of **idle**
    // sources (DEFAULT_IDLE = 200) — so acking here would jump the queue past
    // idle work enqueued *before* the command arrived, which is exactly the
    // false-pass the barrier exists to prevent. Instead the refresh + ack move
    // into an idle callback of their own: glib dispatches same-priority idles
    // FIFO, so every idle queued earlier has run by the time ours does.
    if fauna_e2e_agent::command_needs_barrier(&cmd.action, &cmd.payload) {
        let shared = std::sync::Arc::clone(shared);
        let stack = current_stack.clone();
        let app_state = current_app_state.clone();
        let error_label = current_error_label.clone();
        let id = cmd.id.clone();
        glib::idle_add_local_once(move || {
            // Every idle queued before this one has now run. Freeze what the
            // barrier saw BEFORE the ack — linux keeps republishing state on its
            // 50 ms tick, so only this frozen copy survives to the driver's read
            // (`fauna_e2e_agent::BARRIER_ACK_PROBE_KEY`).
            automation::link::freeze_barrier_ack_probe();
            main_loop_meter::dispatch(
                "agent-barrier-ack",
                || "barrier".to_string(),
                || {
                    update_shared_state(
                        &shared,
                        &stack.borrow(),
                        &app_state.borrow(),
                        &error_label.borrow(),
                    )
                },
            );
            if let Ok(mut s) = shared.lock() {
                s.last_command_id = id.clone();
                s.ready = true;
            }
            automation::link::record_ack(&id);
        });
        return;
    }

    // Refresh state_json from live app state *before* acking. The ack
    // (`ready=true` + matching `last_command_id`) is what the test agent's
    // `await_ack` poll thread waits on to push state to the bridge, and the
    // driver returns from `set_state`/`call_command` the instant it sees that
    // ack, then reads the pushed state. If the ack were set before rebuilding
    // state_json (which otherwise happens only later, in the 50ms GTK tick's
    // `update_shared_state` after the command drain), `await_ack` could push a
    // stale prior-tick state_json carrying the new last_command_id — so the
    // driver reads pre-command state. Rebuilding here makes the ack a true
    // barrier: it always reflects every mutation applied before this command
    // (e.g. an add-participant confirm click processed just before the
    // `list_threads()` empty patch). Uniform across all state reads.
    ack_applied_command(
        shared,
        current_stack,
        current_app_state,
        current_error_label,
        &cmd.id,
    );
    // Diagnostic for the ack-timeout flakiness track: log how long the GTK
    // main thread spent in this handler. A value approaching the driver's 10s
    // ack budget pinpoints the stall as in the handler itself (heavy view
    // build, WS reconnect) rather than in the push pipeline (now eager — see
    // `test_agent::await_ack`).
    tracing::debug!(
        "[TestAgent] handle_test_command done: action={} elapsed_ms={}",
        cmd.action,
        started.elapsed().as_millis()
    );
}

/// Refresh the published state, THEN ack `id` — the order that makes an ack a
/// barrier (see the call at the end of `handle_test_command`). Shared by that
/// call and by the commands whose effect lands later than their handler (the
/// `reset` arm's continuation).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn ack_applied_command(
    shared: &std::sync::Arc<std::sync::Mutex<test_agent::SharedState>>,
    current_stack: &Rc<std::cell::RefCell<Option<gtk::Stack>>>,
    current_app_state: &Rc<std::cell::RefCell<Option<Rc<std::cell::RefCell<app::AppState>>>>>,
    current_error_label: &Rc<std::cell::RefCell<Option<gtk::Label>>>,
    id: &str,
) {
    update_shared_state(
        shared,
        &current_stack.borrow(),
        &current_app_state.borrow(),
        &current_error_label.borrow(),
    );
    // Ack AFTER the refresh, so the driver only sees the ack once the pushed
    // state reflects the command's (and prior UI clicks') effects.
    if let Ok(mut s) = shared.lock() {
        s.last_command_id = id.to_string();
        s.ready = true;
    }
    // Also record the ack process-wide. This stage's `SharedState` may already be
    // orphaned — a stage swap (launch screen → authenticated window, e.g. on a
    // credentialed relaunch) re-points the agent link at a fresh one, and the
    // driver polls *that*. See `automation::link::ACK`.
    automation::link::record_ack(id);
}

// ---------------------------------------------------------------------------
// Sync-folders e2e: seed the Settings → Sync bound-folder list. Same flat
// `{"locations": [{"path", "folder"?, "mode"?}]}` shape as the windows leg's
// `sync_inject_locations` TestAgent command (the Python action layer is uniform).
// `mode` is ignored: an injected row is render-only and never reaches the
// agent, so it renders no `folder-location-mode-toggle` — the switch reads the
// agent's own record (`sync_agent::mode_toggle_for`), which the real-agent
// tests drive.
// ---------------------------------------------------------------------------

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_sync_inject_locations(cmd: &test_agent::RawCommand) {
    let Some(locations) = cmd.payload.get("locations").and_then(|v| v.as_array()) else {
        tracing::debug!("[TestAgent] sync_inject_locations: missing locations array");
        return;
    };
    let mappings: Vec<crate::sync::LocationBinding> = locations
        .iter()
        .filter_map(|f| {
            let path = f.get("path").and_then(|v| v.as_str())?;
            // `folder` omitted/None ⇒ unbound (empty); the host never
            // manufactures a name (file-sync.md § Hosting multiple on-demand).
            let folder = f
                .get("folder")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // An injected row is render-only (it swaps the list the page draws,
            // never reaches the agent), so a seed with no `folder_id` renders
            // with an empty one rather than being dropped.
            let folder_id = f
                .get("folder_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(crate::sync::LocationBinding {
                path: std::path::PathBuf::from(path),
                folder,
                folder_id,
            })
        })
        .collect();
    crate::views::devices_folders::inject_locations_for_test(mappings);
}

/// Inject a `Loaded` feed snapshot built from the command's `posts` array (each a
/// `fauna_feed::test_support::TestPostSpec` shape) onto the live shared
/// `FeedManager`, firing the feed observer so the post list repaints. The shared
/// `test_support` renders each post's `document` from its `body` in Rust, so the
/// Python action sends only plain specs — the browser twin is web's
/// `feed_inject_posts` `__fauna_callCommand` case. The only way to drive the
/// `Failed` unverified-source-badge render (a real signed post is only ever
/// `Unchecked`/`Verified`). No-op before auth — the feed manager is built on the
/// authed connection, so `feed::host::manager()` is `None` pre-auth.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_feed_inject_posts(cmd: &test_agent::RawCommand) {
    let Some(manager) = crate::feed::host::manager() else {
        tracing::debug!("[TestAgent] feed_inject_posts: no feed manager (pre-auth)");
        return;
    };
    let posts = cmd
        .payload
        .get("posts")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
    let specs: Vec<fauna_feed::test_support::TestPostSpec> = match serde_json::from_value(posts) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("[TestAgent] feed_inject_posts: bad posts payload: {e}");
            return;
        }
    };
    manager.set_feed_snapshot_for_test(fauna_feed::test_support::feed_snapshot_with_posts(specs));
}

/// Blocks on the nest `PUT` rather than spawning it: the caller reads the
/// `cues:v1` row straight after the ack, so an ack before the row lands would
/// race it. A refusal lands on the agent-failure slot (convention 11).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_feed_seed_cue_rollup_for_test(
    cmd: &test_agent::RawCommand,
    shared: &std::sync::Arc<std::sync::Mutex<test_agent::SharedState>>,
) {
    const ACTION: &str = "feed_seed_cue_rollup_for_test";
    let Some(manager) = crate::feed::host::manager() else {
        report_agent_command_failure(shared, ACTION, "no feed manager (pre-auth)");
        return;
    };
    let content_ids: Vec<String> = match cmd.payload.get("content_ids") {
        Some(v) => match serde_json::from_value(v.clone()) {
            Ok(ids) => ids,
            Err(e) => {
                report_refused_agent_command(shared, ACTION, &format!("bad content_ids: {e}"));
                return;
            }
        },
        None => Vec::new(),
    };
    if let Err(e) = crate::async_helper::block_on_tokio(async move {
        manager.set_cue_rollup_for_test(content_ids).await
    }) {
        report_agent_command_failure(shared, ACTION, &e);
    }
}

/// Seed a P2P contact directly on the local `PeerDb` (`P2pService::add_contact`
/// — the only caller since the Accept-Invite dialog was retired, p2p.md § No
/// pairing step, ever). `apps/fauna-linux/settings/p2p_tab.rs::refresh_contacts_list`
/// is called separately by the page itself when it next builds/refreshes; this
/// command only writes the row. Payload: `{"actor_id_hex": <64 hex chars>,
/// "display_name": <string>}` — `actor_id_hex` must be exactly 32 bytes hex
/// (fixture setup, not the mutation under test — apps row 128).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_p2p_seed_contact_for_test(cmd: &test_agent::RawCommand) {
    let Some(p2p) = crate::settings::get_p2p() else {
        tracing::debug!("[TestAgent] p2p_seed_contact_for_test: no P2pService (pre-auth)");
        return;
    };
    let actor_id_hex = cmd
        .payload
        .get("actor_id_hex")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let actor_id = match fauna_core::hex32::decode(actor_id_hex) {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!("[TestAgent] p2p_seed_contact_for_test: bad actor_id_hex: {e}");
            return;
        }
    };
    let display_name = cmd
        .payload
        .get("display_name")
        .and_then(|v| v.as_str())
        .unwrap_or("Test Peer")
        .to_string();
    let contact = fauna_peer::contact::PeerContact {
        actor_id,
        display_name,
        p2p_enabled: true,
        ..Default::default()
    };
    if let Err(e) = p2p.add_contact(&contact) {
        tracing::debug!("[TestAgent] p2p_seed_contact_for_test: add_contact failed: {e}");
    }
}

// ---------------------------------------------------------------------------
// Unified-conversations e2e bridge command handlers. Mirror
// ConversationsCommands.cs on Windows; same JSON shape so the
// cross-app action layer drives both unchanged.
// ---------------------------------------------------------------------------

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_conversations_inject_inbound(cmd: &test_agent::RawCommand) {
    // Windows TestAgent.cs passes the WHOLE command dict (with rail /
    // sender / subject / body at the top level) to InjectInbound. The
    // Python action layer mirrors that with `body.update(payload)`, so
    // the fields land at the top of the JSON, not under a `state`
    // envelope. Read from `cmd.payload` to match.
    let Some(p) = cmd.payload.as_object() else {
        tracing::debug!("[TestAgent] conversations_inject_inbound: missing payload");
        return;
    };
    let m = crate::conversations::manager();
    if let Err(e) = m.inject_inbound_from_test_payload(p) {
        tracing::debug!("[TestAgent] conversations_inject_inbound: {e}");
    }
}

/// Seed a pre-resolved link-preview (render-model.md § D4) onto the manager so an
/// injected bubble's `RenderBlock::LinkPreview` block folds `Resolved` — the
/// conversations twin of the feed `link_preview` inject spec, and the e2e path for
/// `test_conversations_link_preview.py`. The resolve is mocked (a real one needs a live
/// nest OpenGraph fetch, SSRF-guarded); the manager's `thread_detail` fold paints the
/// `link-preview-card` (title/description/domain), with the og:image gated behind the
/// message's `load-remote-content-button` reveal. Flat payload shape (see
/// `handle_conversations_inject_inbound`): `{url, title, description, image_hash?}`.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_conversations_seed_resolved_link_preview(cmd: &test_agent::RawCommand) {
    let Some(p) = cmd.payload.as_object() else {
        tracing::debug!("[TestAgent] conversations_seed_resolved_link_preview: missing payload");
        return;
    };
    let Some(url) = p.get("url").and_then(|v| v.as_str()) else {
        tracing::debug!("[TestAgent] conversations_seed_resolved_link_preview: missing url");
        return;
    };
    let title = p
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = p
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let image_hash = p
        .get("image_hash")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    crate::conversations::manager().seed_resolved_link_preview_for_test(
        url.to_string(),
        title,
        description,
        image_hash,
    );
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_conversations_create_mls_group(cmd: &test_agent::RawCommand) {
    use fauna_conversations::{Rail, TypedAddress};
    // Flat-shape command (see handle_conversations_inject_inbound).
    let participants: Vec<TypedAddress> = cmd
        .payload
        .get("participants")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| p.as_str())
                .map(|s| TypedAddress::Fauna {
                    handle: s.to_string(),
                    // ⚠ NOT `ActorId([0u8; 32])`, which this seam minted until
                    // 2026-08-10: that gave every member of a fixture group ONE
                    // identity, so any per-person assertion (a review flag, a
                    // per-member badge) lit up on all of them or on none, and
                    // could not fail for its own reason. The shared helper is
                    // deterministic per handle, so a driver can address a
                    // specific member.
                    actor_id: fauna_conversations::manager::test_actor_id_for_handle(s),
                })
                .collect()
        })
        .unwrap_or_default();
    let _ = Rail::FaunaMls;
    crate::conversations::manager().create_mls_group(participants);
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_conversations_accept_recipient(
    _cmd: &test_agent::RawCommand,
    shared: &std::sync::Arc<std::sync::Mutex<test_agent::SharedState>>,
) {
    // The manager decides which recipient picker is active (add-participant
    // overlay takes priority over new-thread compose) and whether its
    // current text parses. Mirrors Windows' AcceptVisibleRecipientPicker —
    // and, since 2026-08-03, the GUI's own probe-then-commit order: see
    // `conv_backend::e2e_accept_recipient` for why committing without the
    // probe made every Fauna handle/actor id undrivable over the agent.
    //
    // The boolean is convention 11's declining-arm clause: `e2e_accept_recipient`
    // has always returned whether a chip actually committed, and this caller
    // dropped it until 2026-08-28 — so an accept against an untouched picker
    // acked green and surfaced ~5 s later as the action layer's generic "chip
    // not added", naming neither the command nor the reason. tui discarded the
    // same boolean one layer down; web hit it too.
    if !crate::conversations::conv_backend::e2e_accept_recipient() {
        report_agent_command_failure(
            shared,
            "conversations_accept_recipient",
            fauna_conversations::manager::ACCEPT_RECIPIENT_NO_CHIP_REASON,
        );
    }
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_conversations_inject_send_failure(cmd: &test_agent::RawCommand) {
    use fauna_conversations::thread::ThreadId;
    let thread_id = cmd_str(cmd, "thread_id");
    if thread_id.is_empty() {
        tracing::debug!("[TestAgent] conversations_inject_send_failure: missing thread_id");
        return;
    }
    let reason = cmd_str(cmd, "reason");
    let reason = if reason.is_empty() {
        "nest rejected fauna.email.send".to_string()
    } else {
        reason.to_string()
    };
    crate::conversations::manager()
        .inject_send_failure_for_test(&ThreadId(thread_id.to_string()), reason);
}

/// `{thread_id, filename}` → the shared `evict_thread_attachments_for_test`, which
/// redraws. Nothing evicted is a FAILED command, never an ack (convention 11): a
/// render asserted after a no-op evict witnesses nothing. The tui twin is its
/// `conversations_evict_attachment` arm in `automation.rs`.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_conversations_evict_attachment(
    cmd: &test_agent::RawCommand,
    shared: &std::sync::Arc<std::sync::Mutex<test_agent::SharedState>>,
) {
    use fauna_conversations::thread::ThreadId;
    let thread_id = cmd_str(cmd, "thread_id");
    let filename = cmd_str(cmd, "filename");
    let evicted = crate::conversations::manager()
        .evict_thread_attachments_for_test(ThreadId(thread_id.to_string()), filename.to_string());
    if evicted == 0 {
        report_agent_command_failure(
            shared,
            "conversations_evict_attachment",
            &format!("no resident attachment named {filename:?} in thread {thread_id:?}"),
        );
    }
}

/// The paint and the scroll-into-view are the detail view's own reaction to
/// the manager's `notify` (`views/conversations/detail.rs`), exactly as for a
/// Search `Mail` result; this only makes the call.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_conversations_select_message(
    cmd: &test_agent::RawCommand,
    shared: &std::sync::Arc<std::sync::Mutex<test_agent::SharedState>>,
) {
    let thread_id = cmd_str(cmd, "thread_id");
    let message_id = cmd_str(cmd, "message_id");
    if thread_id.is_empty() || message_id.is_empty() {
        report_refused_agent_command(
            shared,
            "conversations_select_message",
            "missing thread_id/message_id",
        );
        return;
    }
    crate::conversations::manager().select_thread_and_message(
        fauna_conversations::ThreadId(thread_id.to_string()),
        fauna_conversations::message::MessageId(message_id.to_string()),
    );
}

/// Stamp `ConversationsSnapshot.error` — the membership/label twin of
/// [`handle_conversations_inject_send_failure`], same reason: no product
/// path fails a `confirm_add_participant`/`remove_participant`/`rename_thread`
/// on demand (`conversations.md` § Errors & edge cases).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_conversations_inject_page_error(cmd: &test_agent::RawCommand) {
    let key = cmd_str(cmd, "key");
    let key = if key.is_empty() {
        "conversations.unified.error_add_participant".to_string()
    } else {
        key.to_string()
    };
    let message = cmd_str(cmd, "message");
    crate::conversations::manager().inject_page_error_for_test(
        fauna_core::localized::LocalizedText::key_arg(key, "message", message),
    );
}

// --- Real-wire FaunaMls e2e drivers ---
//
// Thin payload-parsing wrappers over `conv_backend::e2e_*`, which `block_on` the
// real backend's async manager wire-drivers. Each failure goes to
// `report_agent_command_failure` — the app's own `error-message`, ahead of any
// page's error (`e2e-conventions.md` § convention 11). They used to go to
// `tracing::error!` while the agent acked success, on the theory that "the
// test's proof is the observable nest effect". It is not enough: a nest
// `forbidden` then surfaces two assertions later as a *missing* effect, which
// reads as a half-completed MLS bootstrap rather than a policy refusal — the
// exact swallow that cost a whole triage pass (2026-08-03).

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn cmd_str<'a>(cmd: &'a test_agent::RawCommand, key: &str) -> &'a str {
    cmd.payload.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

/// [`fauna_e2e_agent::SHARE_SERVE_TALLY_KEY`]'s body, or `null` on a build
/// without the share plane (an absent leg, never an empty tally). tui's twin
/// is `automation.rs::share_serve_tally_json`.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn share_serve_tally_json() -> serde_json::Value {
    #[cfg(feature = "p2p-share")]
    {
        serde_json::to_value(fauna_sync_engine::share_serve_tally::snapshot())
            .unwrap_or(serde_json::Value::Null)
    }
    #[cfg(not(feature = "p2p-share"))]
    {
        serde_json::Value::Null
    }
}

/// Stamp the nav-independent agent-failure slot the state serializer reads into
/// `messages.error` ahead of every on-screen error. The tui twin is
/// `App::report_failed_agent_command`.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn report_agent_command_failure(
    shared: &std::sync::Arc<std::sync::Mutex<test_agent::SharedState>>,
    action: &str,
    reason: &str,
) {
    let text = format!("test agent command {action:?} failed: {reason}");
    tracing::error!("[conv-backend] {text}");
    if let Ok(mut s) = shared.lock() {
        s.agent_command_failure = Some(text);
    }
}

/// The active window's split view, for the two walk-command arms — see
/// [`crate::app::split_view_of_window`] for why the widget path is spelled once.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn split_view_of(application: &adw::Application) -> Option<adw::OverlaySplitView> {
    application
        .active_window()
        .and_then(|w| w.downcast::<adw::ApplicationWindow>().ok())
        .and_then(|w| crate::app::split_view_of_window(&w))
}

/// Stamp the same slot for a command linux **never tried** — an action with no
/// arm, or one whose payload it rejected. The tui twin is
/// `App::report_refused_agent_command`.
///
/// ⚠ **This exists because linux was the fleet's one silent agent.** Its single
/// `match cmd.action.as_str()` ended in a bare `_ => {}` until 2026-08-16, so an
/// unimplemented command acked green and was observable nowhere — the exact
/// "bare `return`" `e2e-conventions.md` § convention 11 forbids, and the one
/// failure mode that cannot be caught downstream, because it surfaces as
/// whatever assertion happens to run next in whatever feature the *next* command
/// belonged to. Surveyed the same day, the other six agents all refused loudly
/// (tui's `apply_command` wrapper, web's registry miss, android's `else ->`,
/// apple's `testAgentFailure`, windows' `default:`); linux was the blind spot,
/// and being the only one made it invisible rather than obvious.
///
/// The floor is pinned cross-app by
/// `tests/test_agent_refuses_unknown_command.py`; keep the action name in the
/// text, because a walk driving many commands needs to know *which* one was
/// refused.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn report_refused_agent_command(
    shared: &std::sync::Arc<std::sync::Mutex<test_agent::SharedState>>,
    action: &str,
    reason: &str,
) {
    let text = format!("test agent refused command {action:?}: {reason}");
    tracing::error!("[TestAgent] {text}");
    if let Ok(mut s) = shared.lock() {
        s.agent_command_failure = Some(text);
    }
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_conversations_real_resolve_send_new(
    cmd: &test_agent::RawCommand,
    shared: &std::sync::Arc<std::sync::Mutex<test_agent::SharedState>>,
) {
    if let Err(e) = crate::conversations::conv_backend::e2e_resolve_send_new(
        cmd_str(cmd, "recipient"),
        cmd_str(cmd, "body"),
    ) {
        report_agent_command_failure(shared, "conversations_real_resolve_send_new", &e);
    }
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_conversations_real_send(
    cmd: &test_agent::RawCommand,
    shared: &std::sync::Arc<std::sync::Mutex<test_agent::SharedState>>,
) {
    if let Err(e) = crate::conversations::conv_backend::e2e_send(
        cmd_str(cmd, "thread_id"),
        cmd_str(cmd, "body"),
    ) {
        report_agent_command_failure(shared, "conversations_real_send", &e);
    }
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_conversations_real_send_attachment(
    cmd: &test_agent::RawCommand,
    shared: &std::sync::Arc<std::sync::Mutex<test_agent::SharedState>>,
) {
    use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
    let bytes = match B64.decode(cmd_str(cmd, "data_base64")) {
        Ok(b) => b,
        Err(e) => {
            report_agent_command_failure(
                shared,
                "conversations_real_send_attachment",
                &format!("bad base64: {e}"),
            );
            return;
        }
    };
    if let Err(e) = crate::conversations::conv_backend::e2e_send_with_attachment(
        cmd_str(cmd, "thread_id"),
        cmd_str(cmd, "body"),
        cmd_str(cmd, "filename"),
        cmd_str(cmd, "mime_type"),
        bytes,
    ) {
        report_agent_command_failure(shared, "conversations_real_send_attachment", &e);
    }
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_conversations_real_add(
    cmd: &test_agent::RawCommand,
    shared: &std::sync::Arc<std::sync::Mutex<test_agent::SharedState>>,
) {
    if let Err(e) = crate::conversations::conv_backend::e2e_add(
        cmd_str(cmd, "thread_id"),
        cmd_str(cmd, "peer_actor_id_hex"),
        cmd_str(cmd, "peer_handle"),
    ) {
        report_agent_command_failure(shared, "conversations_real_add", &e);
    }
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_conversations_real_remove(
    cmd: &test_agent::RawCommand,
    shared: &std::sync::Arc<std::sync::Mutex<test_agent::SharedState>>,
) {
    if let Err(e) = crate::conversations::conv_backend::e2e_remove(
        cmd_str(cmd, "thread_id"),
        cmd_str(cmd, "peer_actor_id_hex"),
        cmd_str(cmd, "peer_handle"),
    ) {
        report_agent_command_failure(shared, "conversations_real_remove", &e);
    }
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle_conversations_real_rename(
    cmd: &test_agent::RawCommand,
    shared: &std::sync::Arc<std::sync::Mutex<test_agent::SharedState>>,
) {
    if let Err(e) = crate::conversations::conv_backend::e2e_rename(
        cmd_str(cmd, "thread_id"),
        cmd_str(cmd, "label"),
    ) {
        report_agent_command_failure(shared, "conversations_real_rename", &e);
    }
}

// ---------------------------------------------------------------------------
// State serializer — reads live app state into the shared JSON.
// ---------------------------------------------------------------------------

/// Build the `data.sync` agent-state object: the recent sync-files list plus the
/// external agent's live `running` flag (≥1 serving engine, read over the
/// per-user socket) and the binding model's rendered folder map. Global (not
/// from `AppState`), so they hold across the three arms below — the e2e
/// live-apply test reads `running` + `locations` after a `sync_add_location` /
/// `sync_remove_location` command.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn sync_state_json(files: serde_json::Value) -> serde_json::Value {
    let locations: Vec<serde_json::Value> = crate::sync_agent::current_locations()
        .iter()
        .map(|m| {
            serde_json::json!({
                "path": m.path.display().to_string(),
                "folder": m.folder,
            })
        })
        .collect();
    serde_json::json!({
        "files": files,
        "running": crate::sync_agent::sync_running(),
        "locations": locations,
    })
}

/// If the Settings shell is showing the **Devices** or **Folders** sub-page,
/// return its canonical view name (`"devices"` / `"folders"`) for the nav state
/// report; else `None` (the caller reports plain `"settings"`). Walks the settings
/// shell child for its inner sub-`gtk::Stack` — the same walk the nav-patch handler
/// uses — and reads its visible sub-page. This is what makes the 2026-06-28 nav
/// contract's round-trip assertions pass (the two sub-pages are reached via
/// `{"view":"devices"}` / `{"view":"folders"}` or the two-element settings nav).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn settings_subpage_canonical(stack: &gtk::Stack) -> Option<String> {
    let shell_child = stack.child_by_name("settings")?;
    let mut child = shell_child.first_child();
    while let Some(w) = child {
        if let Ok(inner) = w.clone().downcast::<gtk::Stack>() {
            let sub = inner.visible_child_name()?;
            return match sub.as_str() {
                "devices" | "folders" => Some(sub.to_string()),
                _ => None,
            };
        }
        child = w.next_sibling();
    }
    None
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn update_shared_state(
    shared: &std::sync::Arc<std::sync::Mutex<test_agent::SharedState>>,
    stack: &Option<gtk::Stack>,
    app_state: &Option<Rc<std::cell::RefCell<app::AppState>>>,
    error_label: &Option<gtk::Label>,
) {
    use serde_json::json;

    // Session — read from test overrides (fast, no D-Bus).
    // We don't read from the keyring here because load_credentials()
    // creates a tokio runtime + D-Bus connection on every call, which
    // is too expensive for a 50ms poll tick. The test framework sets
    // session state via overrides; real app state comes from AppState.
    //
    // `authenticated`: an explicit set_state override wins; otherwise the flag
    // is whether the authenticated main window is mounted (`app_state` bound).
    // Stored credentials alone never imply true — the launch machine can route
    // a credentialed relaunch to the wizard, where the session does not exist.
    let mut authenticated_override: Option<bool> = None;
    let (mut node_url, mut secret_hex, mut device_id) = (None, None, None);
    let (mut actor_id_override, mut handle_override) = (None::<String>, None::<String>);
    if let Ok(s) = shared.lock()
        && let Some(ref ov) = s.session_override
    {
        if let Some(v) = ov.authenticated {
            authenticated_override = Some(v);
        }
        if let Some(ref v) = ov.node_url {
            node_url = Some(v.clone());
        }
        if let Some(ref v) = ov.secret_hex {
            secret_hex = Some(v.clone());
        }
        if let Some(ref v) = ov.device_id {
            device_id = Some(v.clone());
        }
        if let Some(ref v) = ov.actor_id {
            actor_id_override = Some(v.clone());
        }
        if let Some(ref v) = ov.handle {
            handle_override = Some(v.clone());
        }
    }

    // Nav — read from the GTK stack.
    let current_view = match stack {
        Some(s) => match s.visible_child_name() {
            // When the Settings shell is showing, map the active Devices / File
            // sets sub-page back to its canonical view so a round-trip nav
            // assertion passes (navigate {"view":"devices"} → read back "devices");
            // other settings sub-pages report plain "settings".
            Some(name) if name == "settings" => {
                settings_subpage_canonical(s).unwrap_or_else(|| "settings".to_string())
            }
            Some(name) => test_agent::gtk_to_canonical(name.as_str()).to_string(),
            None => "conversations".to_string(),
        },
        None => "welcome".to_string(),
    };

    // Data — read from AppState if available, with session overrides.
    let (
        actor_id,
        handle,
        conversations_json,
        feed_json,
        contacts_json,
        knocks_json,
        events_json,
        notifications_json,
        sync_json,
    ) = match app_state {
        Some(rc) => {
            if let Ok(app) = rc.try_borrow() {
                let aid = actor_id_override.or_else(|| {
                    if app.actor_id.is_empty() {
                        None
                    } else {
                        Some(app.actor_id.clone())
                    }
                });
                let hdl = handle_override.or_else(|| {
                    if app.handle.is_empty() {
                        None
                    } else {
                        Some(app.handle.clone())
                    }
                });

                // `data.conversations` is null: the legacy fauna-native inbox
                // drain that fed it was removed (WS-RPC-everywhere rip-out).
                // Conversation data now lives in `data.conversation_threads`
                // (off the shared `ConversationsManager` snapshot, below) —
                // matching Apple. `CAPABILITIES["linux"]["conversations"]` is
                // False accordingly.

                // Feed — straight off the shared `FeedManager` snapshot
                // (`PostSummary`), the single post-list state. `tags` is the
                // nest's facet list; `media_hash` is resolved lazily by
                // `FeedManager::resolve_media` ("" until it lands). `None`
                // before auth ⇒ an empty list.
                let posts: Vec<serde_json::Value> = crate::feed::host::manager()
                    .map(|m| m.snapshot().posts)
                    .unwrap_or_default()
                    .iter()
                    .map(|p| {
                        json!({
                            "post_id": p.post_id,
                            "author": p.author,
                            "body": p.body,
                            "timestamp": p.timestamp,
                            "tags": p.tags,
                            "has_media": p.has_media,
                            // Read by the e2e helper post_image_blob_hash_by_text
                            // (expects 64 hex).
                            "media_hash": p.media_hash.clone().unwrap_or_default(),
                            "is_reply": p.is_reply,
                            // The four interaction counts the bar renders
                            // (`feed.md` § Interaction bar). Emitted so the
                            // count is *assertable*: without them the e2e
                            // reader answers `None`, which reads like "no
                            // activity" rather than "this app never told you".
                            "like_count": p.like_count,
                            "reply_count": p.reply_count,
                            "repost_count": p.repost_count,
                            "quote_count": p.quote_count,
                            // The like toggle's viewer state (feed.md
                            // § Interaction bar → Repost). Emitted for the
                            // same reason as the counts: without it the e2e
                            // reader answers `None`, which reads like "not
                            // liked" rather than "this app never told you".
                            "viewer_liked": p.viewer_liked,
                            // The repost carrier + per-viewer pair (feed.md
                            // § Interaction bar → Repost, ratified 2026-08-10).
                            // `reposted_post_id` is how the harness tells a
                            // repost row from an empty quote until it can read
                            // the `repost-attribution` element directly;
                            // `viewer_repost_id` is the toggle's state (and
                            // `unrepost`'s argument).
                            "reposted_post_id": p.reposted_post_id,
                            "viewer_repost_id": p.viewer_repost_id,
                            // Every link preview in the body with its state,
                            // in body order (`RenderDocument::link_previews` —
                            // render-model.md § D4). A card is absent while a
                            // preview is still resolving too, so this is what
                            // lets a test wait until a preview has FAILED
                            // before it reads "no card". tui's key.
                            "link_previews": p
                                .document
                                .link_previews()
                                .into_iter()
                                .map(|(url, state)| json!({ "url": url, "state": state.name() }))
                                .collect::<Vec<_>>(),
                        })
                    })
                    .collect();

                // Contacts — real fields from ContactRow.
                let contacts: Vec<serde_json::Value> = app
                    .contacts
                    .iter()
                    .map(|c| {
                        json!({
                            "peer_id": c.peer_actor_id,
                            "status": c.status,
                            "handle": c.peer_handle,
                        })
                    })
                    .collect();

                // Knocks — incoming contact requests.
                let knocks: Vec<serde_json::Value> = app
                    .knocks
                    .iter()
                    .map(|k| {
                        json!({
                            "peer_id": k.peer_actor_id,
                            "summary": k.summary,
                            "timestamp": k.timestamp,
                        })
                    })
                    .collect();

                // Events — from the most recent EventsLoaded.
                let events: Vec<serde_json::Value> = app
                    .events
                    .iter()
                    .map(|e| {
                        json!({
                            "id": e.id,
                            "summary": e.summary,
                            "start": e.start_time,
                            "end": e.end_time,
                            "rsvp_status": null,
                        })
                    })
                    .collect();

                // Notifications — unread count.
                let notifs = json!({"unread_count": app.notifications_unread_count});

                // Sync — files from the most recent SyncFilesLoaded.
                let sync_files: Vec<serde_json::Value> = app
                    .sync_files
                    .iter()
                    .map(|f| {
                        json!({
                            "path": f.path,
                            "folder": f.folder,
                            "state": f.state,
                        })
                    })
                    .collect();
                let sync = sync_state_json(json!(sync_files));

                (
                    aid,
                    hdl,
                    serde_json::Value::Null,
                    json!({"posts": posts}),
                    json!(contacts),
                    json!(knocks),
                    json!(events),
                    notifs,
                    sync,
                )
            } else {
                (
                    actor_id_override,
                    handle_override,
                    json!([]),
                    json!({"posts": []}),
                    json!([]),
                    json!([]),
                    json!([]),
                    json!({"unread_count": 0}),
                    sync_state_json(json!([])),
                )
            }
        }
        None => (
            actor_id_override,
            handle_override,
            json!([]),
            json!({"posts": []}),
            json!([]),
            json!([]),
            json!([]),
            json!({"unread_count": 0}),
            sync_state_json(json!([])),
        ),
    };

    // Read error text from the persistent app-level banner if visible; else
    // fall back to a currently-mapped page-level `error-message` widget (e.g. a
    // settings sub-page's client-side validation error), so the state protocol's
    // `messages.error` reflects any on-screen error, not only the global banner.
    // Without this, `ActionLayer.error_text()`/`has_error()` (which consult the
    // state protocol first) miss page-level errors and read empty.
    let error_text: serde_json::Value = {
        // A refused/failed agent command outranks every on-screen error, on
        // every page: it means the app never did what the driver asked, so any
        // later product assertion is reading a state the test did not set up.
        // Same precedence, and same reasoning, as tui's `refused_agent_command`.
        let agent_failure = shared
            .lock()
            .ok()
            .and_then(|s| s.agent_command_failure.clone());
        let banner = match error_label {
            Some(label) if label.is_visible() => {
                let text = label.text().to_string();
                (!text.is_empty()).then_some(text)
            }
            _ => None,
        };
        agent_failure
            .or(banner)
            .or_else(|| {
                crate::automation::find::find("error-message")
                    .filter(|w| w.is_mapped())
                    .map(|w| crate::automation::find::text_of(&w))
                    .filter(|t| !t.is_empty())
            })
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null)
    };

    // WS-RPC echo probe result — serialized as `rpc_echo_reply` for
    // tests/e2e-unified/tests/test_sp_linux_ws_rpc_echo.py.
    let rpc_echo_reply = shared
        .lock()
        .ok()
        .and_then(|s| s.rpc_echo_reply.clone())
        .map(|r| match r {
            test_agent::RpcEchoOutcome::Ok { data_hex } => json!({
                "ok": true,
                "data_hex": data_hex,
            }),
            test_agent::RpcEchoOutcome::Err { error } => json!({
                "ok": false,
                "error": error,
            }),
        })
        .unwrap_or(serde_json::Value::Null);

    // CalDAV-mailbox mint result (Slice C) — serialized as `caldav_mailbox_reply`
    // for tests/e2e-unified/tests/test_caldav_autoschedule_mailbox_less.py.
    let caldav_mailbox_reply = shared
        .lock()
        .ok()
        .and_then(|s| s.caldav_mailbox_reply.clone())
        .map(|r| match r {
            test_agent::CalDavMailboxOutcome::Ok => json!({ "ok": true }),
            test_agent::CalDavMailboxOutcome::Err { error } => json!({
                "ok": false,
                "error": error,
            }),
        })
        .unwrap_or(serde_json::Value::Null);

    // WebDAV serve-enable + blob-reconcile result — serialized as
    // `webdav_serve_reply` for tests/e2e-unified/tests/test_webdav_read_write_roundtrip.py.
    let webdav_serve_reply = shared
        .lock()
        .ok()
        .and_then(|s| s.webdav_serve_reply.clone())
        .map(|r| match r {
            test_agent::WebdavServeOutcome::Ok { served_sets } => json!({
                "ok": true,
                "served_sets": served_sets,
            }),
            test_agent::WebdavServeOutcome::Err { error } => json!({
                "ok": false,
                "error": error,
            }),
        })
        .unwrap_or(serde_json::Value::Null);

    // Return value of the most recent `call_machine_method` (the value-returning
    // bridge path). Readers stash a JSON string in `machine_method_result`;
    // parse it back to a `Value` so the driver reads structured JSON (e.g. the
    // provisioning snapshot object), null when the last method was a setter.
    let machine_method_result = shared
        .lock()
        .ok()
        .and_then(|s| s.machine_method_result.clone())
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .unwrap_or(serde_json::Value::Null);

    // Explicit set_state override wins; otherwise "the authenticated main
    // window is mounted" (an onboarding/launch window binds no AppState).
    let authenticated = authenticated_override.unwrap_or(app_state.is_some());

    // Observed here too, as the backstop for an error surface whose text lives
    // in descendant labels (no signal of its own fires when they change).
    automation::observables::observe_painted_errors();
    let painted_errors = automation::observables::painted_errors_json();

    let mut state_json = json!({
        "session": {
            "authenticated": authenticated,
            "node_url": node_url,
            "secret_hex": secret_hex,
            "actor_id": actor_id,
            "handle": handle,
            "device_id": device_id,
        },
        "nav": {
            "stack": [{"view": current_view}],
            "modal": null,
        },
        // This app's transport connection — the tui `connection` twin
        // (`fauna_e2e_agent::CONNECTION_KEY`, which owns the contract). The word
        // is the one the gate itself is deciding against, so the barrier the
        // harness waits on and the desensitizing it is waiting out can never
        // disagree; the online verdict comes from shared Rust, never from here.
        "connection": fauna_e2e_agent::connection_json(crate::offline_gate::connection_state()),
        // Every connection-state report the indicator received and how many
        // changed its word — the tui `connection_reports` twin
        // (`fauna_e2e_agent::CONNECTION_REPORTS_KEY`, which owns the contract).
        fauna_e2e_agent::CONNECTION_REPORTS_KEY: automation::observables::connection_reports_json(),
        // Every error surface a painted frame showed — the tui `painted_errors`
        // twin (`fauna_e2e_agent::PAINTED_ERRORS_KEY`).
        fauna_e2e_agent::PAINTED_ERRORS_KEY: painted_errors,
        // The launch clock this process signs in on — the tui `clock` twin
        // (`fauna_e2e_agent::CLOCK_KEY`, which owns the contract): the
        // wrong-clock launch witness's in-app control that the
        // `FAUNA_E2E_CLOCK_OFFSET_SECS` seed reached this process.
        fauna_e2e_agent::CLOCK_KEY: fauna_e2e_agent::clock_json(
            fauna_launch_machine::launch_clock::clock_offset_secs(),
            fauna_launch_machine::launch_clock::now_secs_or_zero(),
        ),
        // The held session bearer's schedule on this app's own clock — the tui
        // `launch_token` twin (`fauna_e2e_agent::LAUNCH_TOKEN_KEY`, which owns
        // the contract): linux's bearer is the launch machine's, as on tui.
        // Plain field reads off the machine's snapshot (convention 11 corollary).
        fauna_e2e_agent::LAUNCH_TOKEN_KEY: match settings::get_client() {
            Some(c) => {
                let machine = c.launch_machine();
                let expires_at_secs = match machine.snapshot().token {
                    fauna_launch_machine::TokenStatus::Valid { expires_at_secs } => {
                        Some(expires_at_secs)
                    }
                    _ => None,
                };
                fauna_e2e_agent::launch_token_json(
                    expires_at_secs,
                    fauna_launch_machine::launch_clock::now_secs_or_zero(),
                    &machine.own_token_ids(),
                )
            }
            None => fauna_e2e_agent::launch_token_json(None, 0, &[]),
        },
        // The `barrier` self-test's only observable — the tui `barrier_probe`
        // twin (`fauna_e2e_agent::BARRIER`).
        "barrier_probe": automation::link::barrier_probe(),
        // What the barrier saw at its OWN ack, frozen — the only key the
        // self-test asserts (`fauna_e2e_agent::BARRIER_ACK_PROBE_KEY`).
        "barrier_ack_probe": automation::link::barrier_ack_probe(),
        // Teardowns initiated by this process — convention 14's negative-assert
        // observable, the tui `session_generation` twin
        // (`fauna_e2e_agent::SESSION_GENERATION_KEY`). Live by design: a
        // monotonic counter can only reveal more teardowns to a late read.
        "session_generation": automation::link::session_generation(),
        // The critical-alert sweep's pass counters — the tui `alert_sweep_passes`
        // twin (`fauna_e2e_agent::ALERT_SWEEP_PASSES_KEY`), read straight off the
        // shared registry. The counting is in the shared sweep crate, so this is
        // a read of the same two getters every app publishes, not a linux
        // reimplementation.
        "alert_sweep_passes": {
            "started": crate::critical_alerts::registry().sweep_passes_started(),
            "completed": crate::critical_alerts::registry().sweep_passes_completed(),
        },
        // Per-channel counts of inbound MLS commits this device folded in — the
        // tui `mls_folded_commits` twin
        // (`fauna_e2e_agent::MLS_FOLDED_COMMITS_KEY`). Same shape as
        // `alert_sweep_passes` above: the counting and the JSON are both shared,
        // so this is a read, not a linux reimplementation.
        "mls_folded_commits": fauna_conversations::state_json::mls_folded_commits_json_for_session(
            crate::conversations::conv_backend::active_session().as_ref(),
        ),
        // Full receive-loop cycles begun/finished — the tui `conv_receive_cycles`
        // twin (`fauna_e2e_agent::CONV_RECEIVE_CYCLES_KEY`), the completion
        // observable beside the `conv_receive_now` poke. Shared derivation, so
        // this is a read like the two keys above.
        "conv_receive_cycles": fauna_conversations::state_json::conv_receive_cycles_json(
            crate::conversations::conv_backend::active_session().as_ref(),
        ),
        // What the inbound poll did with peer share-endpoint advertisements —
        // the tui `share_endpoints_counts` twin
        // (`fauna_e2e_agent::SHARE_ENDPOINTS_COUNTS_KEY`). Shared derivation
        // like the keys above, so this is a four-atomic read rather than a
        // linux reimplementation.
        "share_endpoints_counts": fauna_conversations::state_json::share_endpoints_counts_json(
            crate::conversations::conv_backend::active_session().as_ref(),
        ),
        // What this process's share plane has SERVED, per path, plus the serve
        // hold's state — the tui `share_serve_tally` twin
        // (`fauna_e2e_agent::SHARE_SERVE_TALLY_KEY`, which owns the contract).
        // A lock-and-clone, legal on the state path (convention 11 corollary);
        // `null` on a build without the plane, never an empty tally.
        fauna_e2e_agent::SHARE_SERVE_TALLY_KEY: share_serve_tally_json(),
        // The new-message OS banners this process actually fired, plus the diff
        // tick counters that make a negative read of them sound — the witness
        // for `conversations` outcome 11
        // (`fauna_e2e_agent::MESSAGE_BANNERS_KEY`, which owns the contract).
        // Recording and JSON both live in shared Rust, so this is a getter read
        // like the keys above, not a linux tally.
        "message_banners": fauna_conversations::notification::message_banners_json(),
        // Full account-plane pump passes begun/finished — the completion
        // observable beside the `account_pump_now` poke
        // (`fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY`). linux publishes it
        // because linux now HOSTS the runtime (`crate::account_runtime`); an
        // app that does not would have to publish nothing at all, since a
        // convention-11 refusal is `null` and never a zero. Shared derivation
        // like the three keys above, so this is a counter read.
        "account_pump_cycles": fauna_client_account_runtime::account_pump_cycles_json(
            crate::account_runtime::handle().as_ref(),
        ),
        // The shared feed manager's reload counters — the `conv_receive_cycles`
        // twin for the feed's re-query funnel
        // (`fauna_e2e_agent::FEED_RELOADS_KEY`, which owns the contract).
        // Counting and JSON both live in shared Rust; a plain atomic read on
        // the state path, like the keys above. Zeros pre-auth (no manager).
        "feed_reloads": fauna_feed::feed_reloads_json(
            crate::feed::host::manager().as_ref().map(|m| m.reload_counts()),
        ),
        // The post-claim serving-enablement step's completion anchor
        // (`fauna_e2e_agent::SERVING_ENABLEMENT_KEY`, which owns the contract).
        // Shared recording + JSON; a lock-and-clone of a handful of records.
        // An empty run list until the wizard's `LoggedIn` handoff runs it — the
        // legitimate "not run yet".
        "serving_enablement":
            fauna_client_mail_settings::serving_enablement::serving_enablement_json(),
        // Children this instance spawned via `account-open-new-instance-button`
        // (account-scoping.md § Concurrent instances). Each record carries the
        // child's own `agent_port`, which is the ONLY way the harness can drive
        // it: the child runs its own automation server, so the parent's port
        // cannot reach it. Same key and shape as apple's
        // `InstanceSpawner.stateRecords()` — one cross-app contract.
        "spawned_instances": instance_remote::spawned_instances(),
        // Raises this instance has served on its per-account activation
        // endpoint (account-scoping.md § the per-(OS login, account) raise
        // channel). The RAISER's side proves only that the endpoint answered;
        // this is the receiving side's witness that the raise actually reached
        // the app — which under e2e nothing else can show, because
        // `show_window` skips `present()` there. Without it a test would pass
        // on a delivered call whose raise went nowhere.
        "raises_served": instance_remote::raises_served(),
        "settings": {
            "inbox_mode": settings::get_inbox_mode(),
        },
        "data": {
            "conversations": conversations_json,
            // Unified conversations page — read off the shared
            // ConversationsManager via the shared serializer (also consumed by
            // tui). Mirrors Windows AppDataSnapshot.GetConversationsThreadsForState.
            "conversation_threads": fauna_conversations::state_json::conversation_threads_json(
                &crate::conversations::manager(),
            ),
            // The list's active order — the rows alone cannot name it (shared
            // `conversation_sort_json`, also published by tui).
            "conversation_sort": fauna_conversations::state_json::conversation_sort_json(
                &crate::conversations::manager(),
            ),
            // The selected thread, so a row click that opened no detail pane
            // names whether it selected anything (shared, also tui's).
            "selected_thread_id": fauna_conversations::state_json::selected_thread_id_json(
                &crate::conversations::manager(),
            ),
            // Whether the e2e real-wire FaunaMls backend opt-in has activated.
            // The tier_3 round-trip polls this after
            // `conversations_enable_real_faunamls`.
            "conv_real_backend_active":
                crate::conversations::conv_backend::is_e2e_real_active(),
            // The last succession's group sweep, parked across the account
            // switch — the tui `succession_sweep` twin, off the SAME shared
            // `SweepStatus::state_json_or_null` (the machine vocabulary, not the
            // human lines the section paints).
            "succession_sweep": settings::recovery_kit::succession_sweep_state_json(),
            // The MEMBER side of a succession — `succession_sweep`'s twin for
            // the seat that *receives* the statement. The sweep above is read
            // on the succeeding client; nothing rendered anywhere reports what
            // the audience's own poll and witness did with what it published,
            // and every failure there leaves the participant row looking
            // untouched. The tui `succession_witness` twin, off the SAME shared
            // `fauna_client_recovery::witness::state_json` renderer.
            "succession_witness":
                crate::conversations::conv_backend::witness_state_json(),
            "feed": feed_json,
            "contacts": contacts_json,
            "knocks": knocks_json,
            "events": events_json,
            "notifications": notifications_json,
            "sync": sync_json,
        },
        "messages": {
            "error": error_text,
            "warning": shared.lock().ok()
                .and_then(|s| s.warning_text.clone())
                .map(serde_json::Value::String)
                .unwrap_or(serde_json::Value::Null),
            "info": shared.lock().ok()
                .and_then(|s| s.info_text.clone())
                .map(serde_json::Value::String)
                .unwrap_or(serde_json::Value::Null),
        },
        "rpc_echo_reply": rpc_echo_reply,
        "caldav_mailbox_reply": caldav_mailbox_reply,
        "webdav_serve_reply": webdav_serve_reply,
        "machine_method_result": machine_method_result,
    });
    // Set after the literal, which sits at `json!`'s macro recursion limit.
    //
    // The Devices/Folders machine's refresh triple — the `feed_reloads` twin
    // for `DevicesMachine::refresh` (`fauna_e2e_agent::DEVICES_REFRESHES_KEY`,
    // which owns the contract). The page's refresh is spawned off its map
    // hook, so the nav ack says nothing about it; this is the anchor. Zeros
    // before the machine exists.
    state_json[fauna_e2e_agent::DEVICES_REFRESHES_KEY] =
        fauna_devices_machine::devices_refreshes_json(
            crate::views::devices_folders::current_refresh_counts(),
        );
    // Convention 17's `region-block-never-silent` counts: the on-screen
    // surfaces' verdict walks vs. the block placeholders painted under the main
    // stack (`region::block_render_json`, tui's twin). Omitted — never `null`,
    // which the invariant reads as a lost shape — before a stack exists.
    if let Some(stack) = stack {
        state_json["region_block_render"] = region::block_render_json(stack);
    }

    if let Ok(mut s) = shared.lock() {
        s.state_json = state_json;
    }
}

/// Does this `set_state({session})` patch name a **different session** than the
/// live one — a different actor, or the same actor on a different nest?
///
/// The linux twin of tui's `session::apply_session_patch` converge arm, and
/// deliberately its exact comparison: tui converges when
/// `s.actor_id == new_actor_id && s.client.nest_url() == node_url`, so linux
/// must *switch* on either half changing. It used to read the secret alone,
/// which made a same-actor-different-nest patch neither a switch (no teardown)
/// nor a build (the branch below is gated on `current_stack.is_none()`, and the
/// shell is up) — the app silently kept serving the old nest while `get_state`
/// reported the new one, the wrong-data shape convention 11 exists to forbid.
///
/// An authenticated patch naming the SAME actor on the SAME nest is the driver
/// replaying a login the app already holds (the pinned-store relaunch shape,
/// where the launch flow's own silent challenge restored this very session
/// before the patch arrived). That converges — no teardown, no rebuild — which
/// keeps a second conversations engine from being opened over the live
/// session's own `mls_state.db`; the landing site guards that too, at
/// `conversations::conv_backend::build_session_engine`.
///
/// A malformed `secret_hex` is not a switch: the same conservative reading the
/// secret-only comparison had, so an unparseable patch never tears the shell
/// down.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn session_patch_switches_session(
    session: &serde_json::Value,
    live_secret: &[u8; 32],
    live_node_url: &str,
) -> bool {
    if session.get("authenticated").and_then(|v| v.as_bool()) != Some(true) {
        return false;
    }
    let actor_changed = session
        .get("secret_hex")
        .and_then(|v| v.as_str())
        .and_then(|hex| fauna_core::hex32::decode(hex).ok())
        .is_some_and(|incoming| &incoming != live_secret);
    let nest_changed = session
        .get("node_url")
        .and_then(|v| v.as_str())
        .is_some_and(|url| url != live_node_url);
    actor_changed || nest_changed
}

/// Every authenticated launch goes through `launch_authenticated`, and that is
/// where the silent sign-in that fills the account cache runs — the
/// conversations session's only source of its `<handle>@<domain>` send address.
/// The post-wizard transition once built its own client, window and pump and
/// skipped it, so an account signed in through the wizard refused every send
/// with `no_handle` until the app was relaunched. Source guards, because every
/// launch path needs a display, a nest and a keyring to run, and the defect was a
/// missing call rather than a wrong value.
#[cfg(test)]
mod launch_funnel_tests {
    /// The body of the first `fn <name>(` in `src`, bounded at its column-0
    /// closing brace — without the bound a needle would also match this test
    /// module's own text further down the file.
    fn fn_body<'a>(src: &'a str, signature: &str) -> &'a str {
        let after = src
            .split_once(signature)
            .unwrap_or_else(|| panic!("`{signature}` is in the file"))
            .1;
        let end = after
            .find("\n}\n")
            .unwrap_or_else(|| panic!("`{signature}` ends with a column-0 brace"));
        &after[..end]
    }

    #[test]
    fn the_post_wizard_launch_goes_through_the_one_authenticated_funnel() {
        let body = fn_body(
            include_str!("views/onboarding/mod.rs"),
            "fn finish_launch_after_signin(",
        );
        assert!(
            body.contains("crate::launch_authenticated("),
            "the post-wizard launch must go through `launch_authenticated`, the one funnel \
             that runs the silent sign-in"
        );
        for built_here in ["FaunaClient::new(", "build_main_window(", "ui_channel("] {
            assert!(
                !body.contains(built_here),
                "the post-wizard launch calls `{built_here}` itself again — a second launch \
                 path is exactly how the silent sign-in was skipped"
            );
        }
    }

    #[test]
    fn the_funnel_signs_in_silently_and_runs_first_setup_before_the_window() {
        let body = fn_body(include_str!("main.rs"), "fn launch_authenticated(");
        assert!(
            body.contains(".silent_sign_in()"),
            "`launch_authenticated` must run the silent sign-in that fills the account cache"
        );
        let first_setup = body
            .find("first_setup(&fauna_client)")
            .expect("`launch_authenticated` runs its `first_setup` hook");
        let window = body
            .find("build_main_window(")
            .expect("`launch_authenticated` builds the main window");
        assert!(
            first_setup < window,
            "`first_setup` is the glue that must land before the window exists"
        );
    }
}

#[cfg(test)]
mod launch_route_tests {
    use super::{LaunchRoute, classify_launch};
    use fauna_launch_machine::PendingInviteRecord;

    const SECRET: &str = "11";
    const NEST: &str = "https://nest.example";

    fn record() -> PendingInviteRecord {
        PendingInviteRecord {
            nest_url: NEST.to_string(),
            handle: "joiner".to_string(),
            request_id: "req-1".to_string(),
            status_json: "{}".to_string(),
        }
    }

    fn creds(node_url: &str) -> Option<(String, String)> {
        Some((node_url.to_string(), SECRET.to_string()))
    }

    /// The row the append + pending-invite adoption's switch target lands on: an
    /// identity with NO nest_url and a pending-invite slot is the wizard at
    /// `invite_request` — never the authenticated rebuild the switch used to run
    /// unconditionally against an empty URL.
    #[test]
    fn a_pending_invite_account_routes_to_its_wizard_not_the_authenticated_rebuild() {
        assert_eq!(
            classify_launch(false, creds(""), || false, || Some(record())),
            LaunchRoute::PendingInvite {
                secret_hex: SECRET.to_string(),
                record: record(),
            }
        );
    }

    #[test]
    fn awaiting_dns_is_tested_before_the_authenticated_row() {
        assert_eq!(
            classify_launch(false, creds(NEST), || true, || None),
            LaunchRoute::AwaitingDns {
                secret_hex: SECRET.to_string()
            }
        );
    }

    #[test]
    fn a_full_identity_is_the_authenticated_row_and_never_reads_the_invite_slot() {
        assert_eq!(
            classify_launch(
                false,
                creds(NEST),
                || false,
                || panic!("case 1 reads no invite slot")
            ),
            LaunchRoute::Authenticated {
                node_url: NEST.to_string(),
                secret_hex: SECRET.to_string()
            }
        );
    }

    #[test]
    fn an_identity_without_nest_or_invite_is_handle_entry() {
        assert_eq!(
            classify_launch(false, creds(""), || false, || None),
            LaunchRoute::HandleEntry {
                secret_hex: SECRET.to_string()
            }
        );
    }

    #[test]
    fn no_secret_is_the_fresh_wizard_and_reads_no_slot() {
        for credentials in [None, Some((NEST.to_string(), String::new()))] {
            assert_eq!(
                classify_launch(
                    false,
                    credentials,
                    || panic!("no identity reads no DNS slot"),
                    || panic!("no identity reads no invite slot"),
                ),
                LaunchRoute::Fresh
            );
        }
    }

    /// The regression this row fixes: `AccountRegistry::active()` reports
    /// nothing for an unreadable/malformed index (by design — it never lies
    /// with a guessed shape), which is indistinguishable from "no identity"
    /// to a caller that only reads credentials. Without this row outranking
    /// every other, a real identity sitting behind an unreadable index would
    /// route to `Fresh` and offer a brand-new identity over it — reads no
    /// slot, exactly like `LaunchMachine::start()`'s own ordering.
    #[test]
    fn an_unreadable_index_outranks_every_other_row() {
        for credentials in [None, creds(NEST), creds("")] {
            assert_eq!(
                classify_launch(
                    true,
                    credentials,
                    || panic!("an index refusal reads no DNS slot"),
                    || panic!("an index refusal reads no invite slot"),
                ),
                LaunchRoute::IndexRefused
            );
        }
    }
}

#[cfg(test)]
mod session_patch_tests {
    use super::session_patch_switches_session;

    const LIVE_SECRET: [u8; 32] = [9u8; 32];
    const LIVE_NEST: &str = "http://127.0.0.1:8080";

    fn patch(authenticated: bool, secret: &[u8; 32], node_url: &str) -> serde_json::Value {
        serde_json::json!({
            "authenticated": authenticated,
            "secret_hex": hex::encode(secret),
            "node_url": node_url,
        })
    }

    /// The linux twin of tui's
    /// `a_same_actor_same_nest_session_patch_converges_without_reestablishing`,
    /// at the door rather than the landing site: a replayed login the app
    /// already holds must not tear the authenticated shell down and rebuild it.
    #[test]
    fn a_same_actor_same_nest_patch_converges_without_reestablishing() {
        assert!(
            !session_patch_switches_session(
                &patch(true, &LIVE_SECRET, LIVE_NEST),
                &LIVE_SECRET,
                LIVE_NEST
            ),
            "a replayed login the app already holds must converge, not rebuild"
        );
    }

    #[test]
    fn a_different_actor_is_a_switch() {
        assert!(session_patch_switches_session(
            &patch(true, &[1u8; 32], LIVE_NEST),
            &LIVE_SECRET,
            LIVE_NEST
        ));
    }

    /// The half the secret-only comparison missed: same actor, different nest
    /// is a different session — tui's converge arm requires BOTH halves to
    /// match, so linux must rebuild here rather than keep serving the old nest.
    #[test]
    fn a_same_actor_on_a_different_nest_is_a_switch() {
        assert!(session_patch_switches_session(
            &patch(true, &LIVE_SECRET, "http://127.0.0.1:9999"),
            &LIVE_SECRET,
            LIVE_NEST
        ));
    }

    /// Gated on `authenticated == true` so we only ever tear down when the
    /// build branch is certain to rebuild.
    #[test]
    fn an_unauthenticated_patch_is_never_a_switch() {
        assert!(!session_patch_switches_session(
            &patch(false, &[1u8; 32], "http://127.0.0.1:9999"),
            &LIVE_SECRET,
            LIVE_NEST
        ));
    }

    /// A patch that omits a half says nothing about it — and an unparseable
    /// secret must not tear the shell down (the conservative reading the
    /// secret-only comparison already had).
    #[test]
    fn an_absent_or_malformed_half_is_not_a_switch() {
        let no_halves = serde_json::json!({ "authenticated": true });
        assert!(!session_patch_switches_session(
            &no_halves,
            &LIVE_SECRET,
            LIVE_NEST
        ));
        let bad_secret = serde_json::json!({
            "authenticated": true,
            "secret_hex": "not-hex",
            "node_url": LIVE_NEST,
        });
        assert!(!session_patch_switches_session(
            &bad_secret,
            &LIVE_SECRET,
            LIVE_NEST
        ));
    }
}
