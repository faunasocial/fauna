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

crate::app::tick_observer! {
    /// Wakes the render loop when a feeder posts or clears. `UiMessage::Noop` is the
    /// channel's own "redraw, no state change" heartbeat — the loop redraws at the
    /// top of every iteration, so delivering *any* message is what makes the new
    /// alert visible.
    pub ChannelObserver,
    CriticalAlertsObserver,
    UiMessage::Noop
}

impl ChannelObserver {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<UiMessage>) -> Self {
        Self { tx }
    }
}

/// Build the registry and subscribe the wake observer. One per `App`.
pub fn registry(tx: tokio::sync::mpsc::UnboundedSender<UiMessage>) -> Arc<CriticalAlerts> {
    let alerts = Arc::new(CriticalAlerts::new());
    alerts.subscribe(Arc::new(ChannelObserver::new(tx)));
    alerts
}

/// The post-auth split's process-wide record (`fauna_client_alert_sweep::LoopClaim`),
/// so every post-auth hook decides its arm by the shared rule and tui keeps no
/// loop record or forget of its own. It forgets an identity by the registry's
/// teardown epoch, which `drop_authenticated_state`'s `clear_all` already bumps.
static LOOP_CLAIM: fauna_client_alert_sweep::LoopClaim<fauna_client::NestClient> =
    fauna_client_alert_sweep::LoopClaim::new();

/// **The post-auth hook's one critical-alert call** (`critical-alerts.md`
/// § Mechanism → *Who runs the detector* / *How often the detector runs*), run
/// by [`crate::session::establish`] and by the converge arm
/// ([`crate::session::apply_session_patch`]) alike: `LoopClaim` decides the arm —
/// a first or identity-changing sign-in starts the re-sweep loop (first pass at
/// once, then every `RE_SWEEP_INTERVAL_SECS` until the identity's teardown), the
/// same identity on the client its live loop rides runs ONE pass (a second loop
/// would only stack), and the same identity on a client its loop does not ride
/// starts a loop that opens on the custody door.
///
/// Best-effort and fire-and-forget: sign-in must not fail, or even wait, because
/// a nest could not answer the recovery plane. Which feeders run, how a failure is
/// handled and what is logged belong to the shared crate. `wake` is the e2e seam
/// (convention 14): a debug/e2e build races the loop's wait against it, a
/// production build runs the clock alone.
pub fn spawn_post_auth_sweep(
    nest: Arc<fauna_client::NestClient>,
    runtime: crate::settings::AccountRuntimeSlot,
    alerts: Arc<CriticalAlerts>,
    actor_id: fauna_core::identity::ActorId,
    wake: SweepWake,
) {
    use fauna_client_alert_sweep::SweepArm;
    // Keyed on the client's own literal URL, which both hooks read the same way.
    let identity = format!("{}@{}", actor_id.to_hex(), nest.nest_url());
    let (arm, ticket) = LOOP_CLAIM.claim(&identity, &nest, &alerts);
    let custody = crate::settings::atproto::identity_door(runtime);
    tokio::spawn(async move {
        let _ticket = ticket;
        match arm {
            SweepArm::OnePass => {
                fauna_client_alert_sweep::run_one_shot_sweep(nest, &custody, &alerts, &actor_id)
                    .await;
            }
            #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
            SweepArm::Loop => {
                fauna_client_alert_sweep::run_alert_sweep_loop_wakeable(
                    nest,
                    &custody,
                    &alerts,
                    &actor_id,
                    move || {
                        let notify = wake.notify();
                        async move { notify.notified().await }
                    },
                )
                .await;
            }
            #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
            SweepArm::ReestablishedLoop => {
                fauna_client_alert_sweep::run_alert_sweep_loop_reestablished_wakeable(
                    nest,
                    &custody,
                    &alerts,
                    &actor_id,
                    move || {
                        let notify = wake.notify();
                        async move { notify.notified().await }
                    },
                )
                .await;
            }
            #[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
            SweepArm::Loop => {
                let _ = wake;
                fauna_client_alert_sweep::run_alert_sweep_loop(nest, &custody, &alerts, &actor_id)
                    .await;
            }
            #[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
            SweepArm::ReestablishedLoop => {
                let _ = wake;
                fauna_client_alert_sweep::run_alert_sweep_loop_reestablished(
                    nest, &custody, &alerts, &actor_id,
                )
                .await;
            }
        }
    });
}

/// What ends the re-sweep loop's wait early — the e2e seam behind the agent's
/// `alert_sweep_wake` command, so a witness of "a condition that arises while
/// the app is open is announced without a restart" drives the production loop
/// rather than a one-shot pass (`critical-alerts.md` § Mechanism → *How often
/// the detector runs*). One per identity: [`crate::session::establish`] mints a
/// fresh one for each session it establishes, so a departed identity's loop —
/// which lingers until its next wake reads the teardown — holds a handle nothing
/// fires any more. The shape is the shared [`fauna_core::loop_wake::LoopWake`].
pub type SweepWake = fauna_core::loop_wake::LoopWake;

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
