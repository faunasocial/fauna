//! The every-page critical-alerts banner — linux's rendering of the
//! cross-page "something is very wrong" surface
//! (`docs/goal/behavior/critical-alerts.md`; ui.yaml `global:` elements
//! `critical-alerts` / `critical-alert[N]`, user-approved 2026-07-23).
//!
//! One app-wide [`CriticalAlerts`] registry (feeder #1: the bluesky
//! genesis-seniority custody check in `fauna-atproto-settings-machine`), one
//! banner strip mounted above the content stack in `build_main_window` — so it
//! is visible on every authenticated page. Non-dismissable by design: an alert
//! disappears only when its condition re-checks clean.
//!
//! Deliberately minimal (destructive-styled labels, no interaction) — and that
//! is the ratified, cross-app-consistent shape, not a placeholder: web's
//! `CriticalAlertsBanner.svelte` and tui's own band are equally plain text,
//! and `critical-alerts.md` § Implementation status today ratifies nothing
//! richer for any of the 7 apps.

use std::sync::{Arc, OnceLock};

use gtk::prelude::*;

use fauna_client_alerts::{CriticalAlerts, CriticalAlertsObserver};

static REGISTRY: OnceLock<Arc<CriticalAlerts>> = OnceLock::new();

/// The app-wide registry: feeders post here (the atproto settings machine
/// receives it at construction), the banner below renders it.
pub fn registry() -> Arc<CriticalAlerts> {
    REGISTRY
        .get_or_init(|| Arc::new(CriticalAlerts::new()))
        .clone()
}

/// Drop every active alert because the **identity is changing** — sign-out,
/// account switch, nest-untrust, factory reset, and the test-agent's reset /
/// actor-switch arms all call this.
///
/// [`registry`] is a process-wide `OnceLock`, so it is an *identity-scoped
/// singleton that survives a window teardown* — the same class as
/// `conversations::manager()` and `feed::host`, and it was missed when those two
/// were wired. Alerts are keyed by the outgoing account's DID
/// (`atproto-custody:<did>`) and nothing re-checks that DID once its machine is
/// gone, so without this the rebuilt banner accuses the *incoming* account —
/// naming a handle the user no longer has, on every page, un-dismissably and
/// permanently. Not a user gesture: alerts are never dismissable
/// (`critical-alerts.md` § Mechanism scopes the registry to the app session).
pub fn clear_for_identity_change() {
    registry().clear_all();
    // The departed identity's loop lingers until its next wake reads the
    // teardown; dropping its wake here means the agent's `alert_sweep_wake`
    // can never be the thing that wakes it (and refuses until the incoming
    // identity's loop mints its own).
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    take_sweep_wake();
}

/// What ends the current identity's re-sweep WAIT early — the e2e seam behind
/// the agent's `fauna_e2e_agent::ALERT_SWEEP_WAKE`, so a witness of "a condition
/// that arises while the app is open is announced without a restart" drives the
/// production loop rather than a one-shot pass (`critical-alerts.md`
/// § Mechanism → *How often the detector runs*). tui's `SweepWake` is the
/// reference; linux keeps it process-wide beside [`registry`] because the
/// registry is.
///
/// One per identity: [`mint_sweep_wake`] runs where the loop is spawned (the
/// AuthSuccess post-auth hook, once per session establishment), replacing the
/// previous identity's handle, and [`clear_for_identity_change`] drops it — so a
/// departed identity's lingering loop holds a `Notify` nothing fires any more.
/// Compiled out of a release artifact (convention 15), where the loop runs on
/// its clock alone.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
static SWEEP_WAKE: std::sync::Mutex<Option<Arc<tokio::sync::Notify>>> = std::sync::Mutex::new(None);

/// Mint the wake for the loop about to be spawned, replacing any earlier one.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn mint_sweep_wake() -> Arc<tokio::sync::Notify> {
    let notify = Arc::new(tokio::sync::Notify::new());
    *SWEEP_WAKE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::clone(&notify));
    notify
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn take_sweep_wake() {
    SWEEP_WAKE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
}

/// End the current identity's re-sweep wait now; `false` when no loop is
/// running for an identity (pre-auth, or just after an identity change), which
/// the agent refuses loudly (convention 11). A wake that lands mid-pass is
/// kept (`notify_one` stores the permit) and ends the very next wait.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn wake_sweep() -> bool {
    match SWEEP_WAKE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
    {
        Some(notify) => {
            notify.notify_one();
            true
        }
        None => false,
    }
}

/// Bridges registry notifications (which may fire on a tokio worker) to the
/// GTK main loop — the `GtkAtprotoSettingsObserver` idiom.
struct ChannelObserver {
    tx: async_channel::Sender<()>,
}

impl CriticalAlertsObserver for ChannelObserver {
    fn on_changed(&self) {
        let _ = self.tx.try_send(());
    }
}

/// Build the banner strip. Mounted once, above the content stack; hidden
/// while no alert is active.
pub fn build_banner() -> gtk::Box {
    let container = gtk::Box::new(gtk::Orientation::Vertical, 4);
    container.set_widget_name("critical-alerts");
    container.add_css_class("critical-alerts-banner");
    container.set_visible(false);

    let (tx, rx) = crate::async_helper::snapshot_wake_channel();
    registry().subscribe(Arc::new(ChannelObserver { tx }));

    let render = {
        let container = container.clone();
        move || {
            while let Some(child) = container.first_child() {
                container.remove(&child);
            }
            let texts = registry().active_lines(crate::i18n::strings::lookup);
            for text in &texts {
                let label = gtk::Label::new(Some(text));
                // Every row shares the `critical-alert` name; tests address
                // them as `critical-alert[N]` (tree order), the post-card
                // convention.
                label.set_widget_name("critical-alert");
                label.set_wrap(true);
                label.set_xalign(0.0);
                container.append(&label);
            }
            container.set_visible(!texts.is_empty());
        }
    };
    render();
    crate::async_helper::spawn_wake_loop(rx, move || {
        render();
        glib::ControlFlow::Continue
    });
    container
}
