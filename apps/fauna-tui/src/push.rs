//! Push notifications on tui — the `ws-device` transport (`common.md` § Push
//! Notifications → *Transports*): tui subscribes this install's row, the per-user
//! sync agent posts the OS notification while tui is closed, and an open tui
//! owns its own banners (`os_notify`) and ignores the frame.
//!
//! Everything stateful is the shared `fauna_client_push::registration` machine
//! (the install intent bit, the which-actor record, the leave-shape drops);
//! this module only names tui's inputs to it:
//!
//! * **the device id** — [`crate::media::device_id_hex`] for the signed-in
//!   actor, the same function the sync agent's capability is minted from
//!   (`sync_agent.rs`), so the id this row is keyed under, the id tui's own
//!   connection announces and the id the agent announces are one value by
//!   construction (`common.md` § Registration → *Every connection announces*);
//! * **the intent store** — one file in the flat, install-scoped config base,
//!   never an account scope (`account-scoping.md` class 2), so no sign-out
//!   erase touches it.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_push::registration::{
    FileIntentStore, PushIntent, PushRegistration, ws_device_subscription,
};

/// The intent file's name under the install-scoped config base.
const INTENT_FILE: &str = "push-intent.cbor";

/// The Settings control's render state.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct PushSettingsState {
    /// The install's stored opt-in bit — what the toggle shows. Written only
    /// from the store (session start, and each toggle's result), never from
    /// the click, so it is evidence of the persisted bit.
    pub(crate) opted_in: bool,
    /// The last toggle's failure, for the inline line.
    pub(crate) error: Option<String>,
}

/// tui's registration: the shared machine over the live connection.
pub(crate) type TuiPushRegistration = PushRegistration<Arc<NestClient>, FileIntentStore>;

/// The install-scoped intent store, or `None` with no config base.
pub(crate) fn intent_store() -> Option<FileIntentStore> {
    crate::session::config_dir().map(|base| FileIntentStore::new(base.join(INTENT_FILE)))
}

/// The stored record (what the toggle renders) — not opted in when unreadable.
pub(crate) fn stored_intent() -> PushIntent {
    use fauna_client_push::registration::IntentStore;
    intent_store().map(|s| s.load()).unwrap_or_default()
}

/// This install's registration for `actor_hex` over `nest`, or `None` when the
/// device id cannot be read (a failure the caller surfaces, never a guessed id:
/// an id that matches no row would make the nest dial elsewhere).
pub(crate) fn registration(nest: Arc<NestClient>, actor_hex: &str) -> Option<TuiPushRegistration> {
    let device_id = crate::media::device_id_hex(actor_hex)?;
    Some(PushRegistration::new(
        nest,
        intent_store()?,
        actor_hex,
        device_id,
    ))
}

/// At sign-in (launch, switch-in): announce this device on every connection,
/// and re-arm the row when this install opted in — never opting it in.
/// Best-effort: a failure is logged, the session proceeds.
pub(crate) async fn on_session_start(nest: Arc<NestClient>, actor_hex: String) {
    let Some(device_id) = crate::media::device_id_hex(&actor_hex) else {
        tracing::warn!("push: no device id for this account; not announcing");
        return;
    };
    nest.set_push_presence(device_id).await;
    if let Some(reg) = registration(nest, &actor_hex)
        && let Err(e) = reg.rearm(ws_device_subscription(reg.device_id())).await
    {
        tracing::warn!("push: re-arm failed: {e}");
    }
}

/// A leave gesture (switch-out, sign-out): drop the leaving actor's row,
/// issued by the leaving session. Best-effort — never a gate.
pub(crate) async fn drop_actor_row(nest: Arc<NestClient>, actor_hex: &str) {
    if let Some(reg) = registration(nest, actor_hex)
        && let Err(e) = reg.drop_actor_row().await
    {
        tracing::warn!("push: dropping the leaving account's row failed: {e}");
    }
}

/// The Settings toggle: on → enable, off → disable. Returns the stored record
/// after the attempt (what the toggle renders — a failed enable leaves it off,
/// a disable clears it even when the nest is unreachable) and the error, if
/// any, for the inline line.
pub(crate) async fn set_opt_in(
    nest: Arc<NestClient>,
    actor_hex: String,
    on: bool,
) -> (PushIntent, Option<String>) {
    let Some(reg) = registration(nest, &actor_hex) else {
        return (
            stored_intent(),
            Some("this device's id could not be read".into()),
        );
    };
    let result = if on {
        reg.enable(ws_device_subscription(reg.device_id())).await
    } else {
        reg.disable().await
    };
    (reg.intent(), result.err().map(|e| e.to_string()))
}

/// The inline line the control paints with nothing failing in-flight: the
/// desktops' two runtime causes (`settings.md` § Push notifications), shown
/// only while this install is opted in — an opted-out install needs no sink.
/// `sink` is the agent's `notification_sink` (`None` = cannot tell, which
/// shows nothing); `agent_running` whether its last status call succeeded.
pub(crate) fn standing_failure(
    opted_in: bool,
    agent_running: bool,
    sink: Option<bool>,
) -> Option<&'static str> {
    use fauna_i18n::strings::settings::push_notifications as t;
    if !opted_in {
        return None;
    }
    if !agent_running {
        return Some(t::AGENT_UNREACHABLE);
    }
    (sink == Some(false)).then_some(t::NO_SINK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_i18n::strings::settings::push_notifications as t;

    #[test]
    fn an_opted_out_install_shows_no_standing_failure() {
        assert_eq!(standing_failure(false, false, Some(false)), None);
    }

    #[test]
    fn an_unreachable_agent_is_named_first() {
        assert_eq!(
            standing_failure(true, false, None),
            Some(t::AGENT_UNREACHABLE)
        );
    }

    #[test]
    fn a_headless_agent_says_no_sink_and_an_older_one_says_nothing() {
        assert_eq!(standing_failure(true, true, Some(false)), Some(t::NO_SINK));
        assert_eq!(standing_failure(true, true, None), None);
        assert_eq!(standing_failure(true, true, Some(true)), None);
    }
}
