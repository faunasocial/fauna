//! The every-page critical-alerts banner — tui's rendering of the cross-page
//! "something is very wrong" surface (`docs/goal/behavior/critical-alerts.md`
//! owns the contract; ui.yaml `global:` owns the `critical-alerts` /
//! `critical-alert[N]` element IDs, user-approved 2026-07-23). The tui twin of
//! `apps/fauna-linux/src/critical_alerts.rs`.
//!
//! Feeder #1 is the ATProto genesis-seniority custody check inside the shared
//! `AtprotoSettingsMachine`, which receives the registry at construction
//! ([`crate::mail_glue::build_atproto_settings_machine`]) — so nothing in this
//! module knows what an alert *means*; it renders whatever the registry holds.
//!
//! Two deliberate differences from linux, both stated where they bite:
//!
//! * **The registry lives on [`crate::app::App`], not in a process-wide
//!   `OnceLock`.** linux can afford a static — one GTK app per process — but
//!   tui's ~900 unit tests all run in ONE process, so a global registry would
//!   leak a posted alert from whichever test wired a feeder into every later
//!   test's assertions. Per-`App` ownership also gives the session-scoped
//!   lifetime the goal doc asks for for free: `drop_authenticated_state` clears.
//! * **Paint is a pull, not a push.** The band re-reads
//!   [`CriticalAlerts::active`] on every frame, so no observer bookkeeping can
//!   desynchronise the pixels from the registry. The observer below exists only
//!   to *wake* the event loop, which otherwise blocks in `select!` until input
//!   arrives — without it an alert raised by a background refresh would sit
//!   unpainted until the user happened to press a key, and "set-and-forget" is
//!   precisely the property this surface exists to serve.

use std::sync::Arc;

use fauna_client_alerts::{CriticalAlerts, CriticalAlertsObserver};

use crate::app::UiMessage;

/// Wakes the render loop when a feeder posts or clears. `UiMessage::Noop` is the
/// channel's own "redraw, no state change" heartbeat — the loop redraws at the
/// top of every iteration, so delivering *any* message is what makes the new
/// alert visible.
pub struct ChannelObserver {
    tx: tokio::sync::mpsc::UnboundedSender<UiMessage>,
}

impl ChannelObserver {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<UiMessage>) -> Self {
        Self { tx }
    }
}

impl CriticalAlertsObserver for ChannelObserver {
    fn on_changed(&self) {
        // A dead receiver means the app is shutting down: nothing to repaint.
        let _ = self.tx.send(UiMessage::Noop);
    }
}

/// Build the registry and subscribe the wake observer. One per `App`.
pub fn registry(tx: tokio::sync::mpsc::UnboundedSender<UiMessage>) -> Arc<CriticalAlerts> {
    let alerts = Arc::new(CriticalAlerts::new());
    alerts.subscribe(Arc::new(ChannelObserver::new(tx)));
    alerts
}

/// Start the critical-alert sweep for the session that just came up
/// (`critical-alerts.md` § Goal — the set-and-forget half).
///
/// This is the call that runs every feeder which must be checked outside its
/// own page: the pending-RecoveryKey-replacement window (whose 30-day alarm was
/// reaching nobody because nothing polled it), the published handle binding,
/// and the genesis-seniority custody check — which also posts from the ATProto
/// settings page's convergence, but only for a user who opens that page.
///
/// It sweeps **immediately and then every
/// `fauna_client_alert_sweep::RE_SWEEP_INTERVAL_SECS`** for as long as the
/// identity lives, so a terminal left open for days keeps re-checking rather
/// than answering once at start (§ Mechanism → *Who runs the detector*). The
/// task needs no cancelling here: it stops itself on the first wake after
/// `drop_authenticated_state` calls `CriticalAlerts::clear_all`.
///
/// Best-effort and fire-and-forget, exactly like the convergence beside it at
/// [`crate::session::establish`] (`spawn_refresh_mail_epoch_schedule`): sign-in must not fail, or even wait, because a nest
/// could not answer the recovery plane. Which feeders run, how often, how a
/// failure is handled, and what gets logged belong to the shared crate, not
/// here — this function is deliberately only a spawn, so the other six apps'
/// copies are the same three lines (priority #1).
pub fn spawn_session_start_sweep(
    nest: Arc<fauna_client::NestClient>,
    runtime: crate::settings::AccountRuntimeSlot,
    alerts: Arc<CriticalAlerts>,
    actor_id: fauna_core::identity::ActorId,
    wake: SweepWake,
) {
    let custody = crate::settings::atproto::identity_door(runtime);
    tokio::spawn(async move {
        // The e2e build runs the SAME loop with its wait raceable by the agent's
        // `alert_sweep_wake` command, so a test drives this loop's own re-sweep
        // instead of waiting out six hours (convention 14). A production build
        // has no wake to race and runs the clock alone.
        #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
        fauna_client_alert_sweep::run_alert_sweep_loop_wakeable(
            nest,
            &custody,
            &alerts,
            &actor_id,
            move || {
                let notify = Arc::clone(&wake.notify);
                async move { notify.notified().await }
            },
        )
        .await;
        #[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
        {
            let _ = wake;
            fauna_client_alert_sweep::run_alert_sweep_loop(nest, &custody, &alerts, &actor_id)
                .await;
        }
    });
}

/// What ends the re-sweep loop's wait early — the e2e seam behind the agent's
/// `alert_sweep_wake` command, so a witness of "a condition that arises while
/// the app is open is announced without a restart" drives the production loop
/// rather than a one-shot pass (`critical-alerts.md` § Mechanism → *How often
/// the detector runs*).
///
/// Empty in a production build (convention 15: the automation surface is
/// compiled out), so the loop there runs on its clock alone. One per identity:
/// [`crate::session::establish`] mints a fresh one for each session it
/// establishes, so a departed identity's loop — which lingers until its next
/// wake reads the teardown — holds a handle nothing fires any more.
#[derive(Clone, Default)]
pub struct SweepWake {
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    notify: Arc<tokio::sync::Notify>,
}

impl SweepWake {
    /// End the current identity's re-sweep wait now. A wake that lands while a
    /// pass is running is kept (`notify_one` stores the permit) and ends the
    /// very next wait, so a test never loses its poke to a busy loop.
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    pub fn wake(&self) {
        self.notify.notify_one();
    }

    /// Test-only: whether a wake is pending for this handle's loop, consuming
    /// it. A zero timeout polls the `Notified` once before its deadline, so a
    /// stored permit answers `true` without any wall clock.
    #[cfg(test)]
    pub(crate) async fn take_pending_wake(&self) -> bool {
        tokio::time::timeout(std::time::Duration::ZERO, self.notify.notified())
            .await
            .is_ok()
    }
}

/// Run **one** sweep pass against the live session's client, at the converge
/// arm of the universal post-auth hook ([`crate::session::apply_session_patch`]).
///
/// A same-actor, same-nest session patch converges on the live session instead
/// of re-running `session::establish` — a second `establish` would open a
/// second conversations engine over the live `mls_state.db` and hit its role
/// lock. That arm still owes the post-auth feeders their re-run, exactly as
/// linux's own converge arm re-runs its four (`critical-alerts.md`
/// § Implementation status today, TRACK 10): without it the sweep answered
/// once per *process* rather than once per session establishment, and anything
/// that changed after the first login — a tampered directory, a replacement
/// window opened mid-session — could never be observed.
///
/// **One shot, never a second loop, and that is the whole point of a separate
/// function.** [`spawn_session_start_sweep`]'s `run_alert_sweep_loop` from the
/// first `establish` is still running (it stops only on the first wake after
/// `CriticalAlerts::clear_all` bumps the teardown epoch, which a converge does
/// not do), so re-firing the loop here would stack one concurrent sweeper per
/// patch, forever. windows' leg ratified this exact split for the same seam
/// (`critical-alerts.md` § Implementation status today: its e2e `session`
/// command "deliberately stays on the one-shot `RunAsync`, since it can re-fire
/// on every e2e login in one process … and stacking a concurrent loop per
/// re-auth was never this leg's job").
///
/// Best-effort and fire-and-forget, same posture as the loop beside it.
pub fn spawn_one_shot_sweep(
    nest: Arc<fauna_client::NestClient>,
    runtime: crate::settings::AccountRuntimeSlot,
    alerts: Arc<CriticalAlerts>,
    actor_id: fauna_core::identity::ActorId,
) {
    let custody = crate::settings::atproto::identity_door(runtime);
    tokio::spawn(async move {
        let report =
            fauna_client_alert_sweep::run_session_start_sweep(nest, &custody, &alerts, &actor_id)
                .await;
        tracing::debug!(
            failures = report.failures.len(),
            skipped = report.skipped.len(),
            "converge-arm critical-alert sweep pass done"
        );
    });
}

/// Word-wrap the alert rows to `width` for paint. The band reserves exactly as
/// many terminal rows as this returns, so the wrap has to happen *here* rather
/// than in `Paragraph::wrap`: ratatui wraps at render time, which would mean
/// guessing the height that its own wrapping is about to need and silently
/// clipping the tail when the guess is low. An alert whose second half
/// ("…contact whoever runs your nest") is cut off is the failure this avoids.
///
/// Greedy on whitespace; a single word longer than `width` is hard-split rather
/// than allowed to overflow. Returns one entry per painted terminal row, which is
/// deliberately NOT one per alert — the registry keeps the logical
/// `critical-alert[N]` rows, exactly as the viewport clips page elements without
/// changing the element list.
pub fn wrapped_lines(lines: &[String], width: u16) -> Vec<String> {
    let width = width.max(1) as usize;
    let mut out = Vec::new();
    for line in lines {
        let mut current = String::new();
        for word in line.split_whitespace() {
            // A word that cannot fit on any line: emit it in width-sized chunks
            // instead of overflowing the band.
            if word.chars().count() > width {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                let chars: Vec<char> = word.chars().collect();
                for chunk in chars.chunks(width) {
                    out.push(chunk.iter().collect());
                }
                continue;
            }
            let projected = if current.is_empty() {
                word.chars().count()
            } else {
                current.chars().count() + 1 + word.chars().count()
            };
            if projected > width {
                out.push(std::mem::take(&mut current));
                current.push_str(word);
            } else {
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(word);
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::localized::LocalizedText;

    fn channel() -> (
        tokio::sync::mpsc::UnboundedSender<UiMessage>,
        tokio::sync::mpsc::UnboundedReceiver<UiMessage>,
    ) {
        tokio::sync::mpsc::unbounded_channel()
    }

    // The mechanism itself (empty-registry, resolve+join, deterministic
    // order) is `fauna_client_alerts::CriticalAlerts::active_lines`'s own
    // coverage now. This test is tui's remaining stake: the REAL i18n table
    // (not a stub lookup) actually resolves the real key this module ships,
    // so a renamed/deleted i18n entry fails here rather than leaking to the
    // screen as a raw key.
    #[test]
    fn an_alert_resolves_through_the_real_i18n_table_and_names_the_handle() {
        let (tx, _rx) = channel();
        let alerts = registry(tx);
        alerts.post(
            "atproto-custody:did:plc:abc",
            vec![LocalizedText::key_arg(
                "critical_alerts.atproto_custody_mismatch",
                "handle",
                "alice@fauna.test",
            )],
        );
        let lines = alerts.active_lines(fauna_i18n::strings::lookup);
        assert_eq!(lines.len(), 1, "one row per active alert");
        assert!(
            !lines[0].contains("critical_alerts."),
            "the row resolves through the i18n table: {:?}",
            lines[0]
        );
        assert!(
            lines[0].contains("alice@fauna.test"),
            "the alert names the affected handle: {:?}",
            lines[0]
        );
    }

    #[test]
    fn a_post_wakes_the_render_loop() {
        let (tx, mut rx) = channel();
        let alerts = registry(tx);
        alerts.post(
            "atproto-custody:did:plc:abc",
            vec![LocalizedText::key("boom")],
        );
        // Without this wake an alert raised by a background refresh would not
        // paint until the user pressed a key.
        assert!(
            matches!(rx.try_recv(), Ok(UiMessage::Noop)),
            "posting an alert must wake the loop"
        );
        alerts.clear("atproto-custody:did:plc:abc");
        assert!(
            matches!(rx.try_recv(), Ok(UiMessage::Noop)),
            "clearing one must repaint too, or a resolved alarm would linger"
        );
    }
}
