//! One gesture on the connected-apps page, as the apps that drive the machine
//! natively (tui, linux) carry it across their async spawn.
//!
//! Both held a hand-copied eight-variant enum and the `match` that forwards each
//! variant to its [`ConnectedAppsMachine`] method; the page's gesture set is the
//! machine's own surface, so it lives beside the machine.

use crate::machine::ConnectedAppsMachine;

/// A gesture on the page: each variant is exactly one machine method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectedAppsGesture {
    /// The nav-edge re-read.
    Refresh,
    SubmitCode(String),
    /// The same-device handoff's open — a `fauna://consent/<request_uri>` route
    /// brought the user here.
    OpenHandoff(String),
    Resolve {
        consent_id_hex: String,
        approved: bool,
    },
    Block(String),
    /// Lift a per-client block, by the blocked client's id.
    Unblock(String),
    Revoke(String),
    /// Set the account's external-apps switch to the carried value
    /// (`atproto-external-apps-enable`).
    SetExternalApps(bool),
}

impl ConnectedAppsGesture {
    /// Run the gesture against `machine`; it ends in the machine's own notify.
    pub async fn run(self, machine: &ConnectedAppsMachine) {
        match self {
            Self::Refresh => machine.refresh().await,
            Self::SubmitCode(code) => machine.submit_code(code).await,
            Self::OpenHandoff(request_uri) => machine.open_handoff(request_uri).await,
            Self::Resolve {
                consent_id_hex,
                approved,
            } => machine.resolve_request(consent_id_hex, approved).await,
            Self::Block(consent_id_hex) => machine.block_request(consent_id_hex).await,
            Self::Unblock(client_id) => machine.unblock(client_id).await,
            Self::Revoke(key) => machine.revoke(key).await,
            Self::SetExternalApps(enabled) => machine.set_external_apps_enabled(enabled).await,
        }
    }
}
