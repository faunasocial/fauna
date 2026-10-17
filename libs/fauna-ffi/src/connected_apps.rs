//! Re-exports the connected-apps page state machine so its UniFFI exports
//! surface in the generated Swift / Kotlin / C# bindings, plus a free-fn
//! constructor that builds the machine over an [`FfiNestClient`]'s WS-RPC
//! connection. The machine itself lives in libs/fauna-client-connected-apps;
//! this file is a thin glue layer (mirrors src/labeler_catalog.rs).

use std::sync::Arc;

use fauna_atproto_settings_machine::ConsentCardRow;
pub use fauna_client_connected_apps::compose::{ComposedText, RosterRowText};
pub use fauna_client_connected_apps::{
    BlockedAppRow, ConnectedAppRow, ConnectedAppsMachine, ConnectedAppsObserver,
    ConnectedAppsSnapshot, MailAppPassword,
};
use fauna_client_mail_settings::MailSettingsMachine;

use crate::FfiNestClient;

/// Build a [`ConnectedAppsMachine`] for the connected-apps page
/// (`docs/goal/ui/connected-apps.md`) over `nest`'s authenticated WS-RPC
/// connection. `observer` ticks on every snapshot change. The machine owns the
/// roster read (`refresh()`), the typed-code start (`submit_code()`), the
/// same-device handoff's open (`open_handoff()`), the Requests tray gestures (`resolve_request()` / `block_request()` /
/// `unblock()`) and the one revoke (`revoke(key)`, whose verb the machine —
/// never the app — picks). Needs no actor secret: every call is a plain
/// authenticated nest request.
///
/// `mail` is the session's own Mail & Calendar machine (the one
/// `build_mail_settings_machine` returned): the mail app passwords are rows of
/// this roster, read, revoked and revealed (`reveal_secret()`) through it.
/// `None` builds a roster without mail rows.
#[uniffi::export]
pub fn build_connected_apps_machine(
    nest: Arc<FfiNestClient>,
    observer: Arc<dyn ConnectedAppsObserver>,
    mail: Option<Arc<MailSettingsMachine>>,
) -> Arc<ConnectedAppsMachine> {
    fauna_client_connected_apps::build_connected_apps_machine(nest.nest_arc(), observer, mail)
}

/// Wire the consent-time grant into a built [`ConnectedAppsMachine`], so an
/// approve of a request naming a `fauna:records:` scope mints the app's grant
/// over its record kinds (`third-party-kinds.md` § The record doors). Needs
/// the actor's 32-byte ed25519 `secret` — the grant is signed into the owner's
/// grant log and its keys derive from that seed — which the builder above
/// deliberately does not take. Without this call such an approve is refused,
/// never resolved keyless. A build without the account runtime refuses here.
#[uniffi::export]
pub fn wire_connected_apps_consent_grant(
    machine: Arc<ConnectedAppsMachine>,
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
) -> Result<(), crate::FfiError> {
    let keypair = crate::keypair_from_bytes(&secret)?;
    #[cfg(feature = "account-runtime")]
    {
        machine.set_consent_grant_seams(crate::atproto_settings::consent_grant_seams(
            &keypair,
            nest.nest_arc(),
        ));
        Ok(())
    }
    #[cfg(not(feature = "account-runtime"))]
    {
        let _ = (machine, nest, keypair);
        Err(crate::FfiError::General {
            msg: "no account runtime in this build".into(),
        })
    }
}

/// A roster row's text, composed once in shared Rust
/// (`fauna_client_connected_apps::compose`; `docs/goal/ui/connected-apps.md`
/// § Where logic lives): the lines top to bottom, then the facts the app joins
/// with the separator as its last line. Each [`ComposedText`] is an i18n key
/// plus arguments — resolve `nested` first, then `text` — through the app's own
/// string table. Instants are formatted in the device's local timezone here.
#[uniffi::export]
pub fn connected_apps_roster_row_text(row: ConnectedAppRow) -> RosterRowText {
    fauna_client_connected_apps::compose::roster_row_text(&row)
}

/// A request card's lines, composed once in shared Rust
/// (`compose::request_card_lines`): the heading, who is asking, the scopes, the
/// permission sets, then what the approve ends besides granting.
#[uniffi::export]
pub fn connected_apps_request_card_lines(consent: ConsentCardRow) -> Vec<ComposedText> {
    fauna_client_connected_apps::compose::request_card_lines(&consent)
}

/// What a roster row's facts line puts between its facts.
#[uniffi::export]
pub fn connected_apps_facts_separator() -> String {
    fauna_client_connected_apps::compose::FACTS_SEPARATOR.to_string()
}
