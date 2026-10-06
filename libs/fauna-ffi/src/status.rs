//! UniFFI façade for the shared **Status snapshot** — the node, sync, MLS and
//! build legs and the seven `status-*` element texts
//! (`docs/goal/ui/status.md` § State & data shape).
//!
//! [`FfiStatusClient::node`] is `fauna_client_status::StatusClient::node`'s one
//! nest read; [`status_text`] is `fauna_client_status::render` — the whole text
//! projection — so an app fetches the legs on its own cadence and paints the
//! returned strings, deriving none of them (`status.md` § Don't do these).
//!
//! The i18n lookup crosses the boundary as a foreign trait
//! ([`FfiStatusLookup`]): the projection resolves `status.sync.pending_summary`,
//! the byte-size and relative-time keys and `common.never` through the *app's*
//! own catalog, so the words are localized exactly like every other string.
//!
//! The build leg is not an input: `status_text` reads the `FAUNA_BUILD_COMMIT`
//! this crate's `build.rs` stamped (`fauna-build-commit`), which is what a
//! native app's "build" means — the shared Rust it links.
//!
//! Gated behind `value-format` (default-on, dropped from the Go mail-bridge
//! `--no-default-features` build): the bridge has no Status surface.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_status::{
    BuildLeg, MlsLeg, NodeLeg, StatusClient, StatusSnapshot, SyncLeg, render,
};

use crate::{FfiError, stringify};

/// The app's i18n lookup, handed to [`status_text`]. Return the localized
/// template for `key`, or `None` when the catalog has no such key (the
/// projection then falls back to the key itself, as every other resolver does).
#[uniffi::export(with_foreign)]
pub trait FfiStatusLookup: Send + Sync {
    fn lookup(&self, key: String) -> Option<String>;
}

/// `status-node-domain` / `status-node-version` — the nest's own
/// `fauna.nest.info` reply.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiNodeLeg {
    pub domain: String,
    pub version: String,
}

impl From<NodeLeg> for FfiNodeLeg {
    fn from(leg: NodeLeg) -> Self {
        Self {
            domain: leg.domain,
            version: leg.version,
        }
    }
}

/// This device's local sync backlog — `status-sync-pending` +
/// `status-sync-last`. Desktop apps only; an app with no local agent passes
/// `None` for the leg.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FfiStatusSyncLeg {
    pub files_pending: u64,
    pub bytes_pending: u64,
    /// Unix **seconds**, `None` before any pass finished.
    pub last_sync: Option<u64>,
}

/// The sync leg from the local agent's per-device signal
/// ([`crate::FfiAgentSyncStatus`], `FfiSyncAgentProvisioner::sync_status`) — the
/// three backlog fields verbatim, as `From<&SyncStatusInfo> for SyncLeg` does on
/// the Rust-native apps.
#[cfg(all(any(unix, windows), feature = "sync-agent-provisioning"))]
#[uniffi::export]
pub fn status_sync_leg(status: crate::FfiAgentSyncStatus) -> FfiStatusSyncLeg {
    FfiStatusSyncLeg {
        files_pending: status.files_pending,
        bytes_pending: status.bytes_pending,
        last_sync: status.last_sync,
    }
}

/// `status-mls-key-packages` / `status-mls-channels`.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FfiMlsLeg {
    /// `fauna.conversations.keypackage.count` for the bearer's own actor.
    pub key_packages: u64,
    /// The conversations manager's `secure_channel_count`.
    pub channels: u64,
}

/// The legs an app has loaded so far — `None` = not loaded, or not applicable
/// here; the matching element is then not rendered.
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq, Eq)]
pub struct FfiStatusLegs {
    pub node: Option<FfiNodeLeg>,
    pub sync: Option<FfiStatusSyncLeg>,
    pub mls: Option<FfiMlsLeg>,
}

/// The seven `status-*` element texts — `None` = do not render the element.
/// Bare values: the labels beside them are the app's own shared `status.*`
/// strings.
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq, Eq)]
pub struct FfiStatusText {
    pub node_domain: Option<String>,
    pub node_version: Option<String>,
    pub sync_pending: Option<String>,
    pub sync_last: Option<String>,
    pub mls_key_packages: Option<String>,
    pub mls_channels: Option<String>,
    pub build_sha: Option<String>,
}

/// The commit this build's shared Rust was compiled from, when stamped.
fn build_leg() -> BuildLeg {
    BuildLeg::from_stamp(option_env!("FAUNA_BUILD_COMMIT"))
}

fn text_for(
    legs: &FfiStatusLegs,
    build: BuildLeg,
    now_ms: i64,
    lookup: &dyn FfiStatusLookup,
) -> FfiStatusText {
    let snapshot = StatusSnapshot {
        node: legs.node.as_ref().map(|n| NodeLeg {
            domain: n.domain.clone(),
            version: n.version.clone(),
        }),
        sync: legs.sync.map(|s| SyncLeg {
            files_pending: s.files_pending,
            bytes_pending: s.bytes_pending,
            last_sync: s.last_sync,
        }),
        mls: legs.mls.map(|m| MlsLeg {
            key_packages: m.key_packages,
            channels: m.channels,
        }),
        build,
    };
    let text = render(&snapshot, now_ms, |key| lookup.lookup(key.to_owned()));
    FfiStatusText {
        node_domain: text.node_domain,
        node_version: text.node_version,
        sync_pending: text.sync_pending,
        sync_last: text.sync_last,
        mls_key_packages: text.mls_key_packages,
        mls_channels: text.mls_channels,
        build_sha: text.build_sha,
    }
}

/// `fauna_client_status::render` — the seven `status-*` element texts for the
/// legs loaded so far, resolved through the app's own catalog. `now_ms` is the
/// app's clock (the `status-sync-last` relative time).
#[uniffi::export]
pub fn status_text(
    legs: FfiStatusLegs,
    now_ms: i64,
    lookup: Arc<dyn FfiStatusLookup>,
) -> FfiStatusText {
    text_for(&legs, build_leg(), now_ms, lookup.as_ref())
}

/// Typed-call client for the Status snapshot's one nest read. Obtain via
/// [`crate::FfiNestClient::status`].
#[derive(uniffi::Object)]
pub struct FfiStatusClient {
    nest: Arc<NestClient>,
}

impl FfiStatusClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }
}

#[fauna_uniffi_async::export]
impl FfiStatusClient {
    /// `fauna.nest.info` → the node leg. Pure read; one per visit to the surface.
    pub async fn node(&self) -> Result<FfiNodeLeg, FfiError> {
        let leg = StatusClient::new(Arc::clone(&self.nest))
            .node()
            .await
            .map_err(stringify)?;
        Ok(leg.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Catalog;
    impl FfiStatusLookup for Catalog {
        fn lookup(&self, key: String) -> Option<String> {
            match key.as_str() {
                "status.sync.pending_summary" => Some("{files} files, {bytes}".into()),
                "common.never" => Some("Never".into()),
                _ => None,
            }
        }
    }

    #[test]
    fn unloaded_legs_render_no_element() {
        let text = text_for(&FfiStatusLegs::default(), BuildLeg::default(), 0, &Catalog);
        assert_eq!(text, FfiStatusText::default());
    }

    #[test]
    fn loaded_legs_project_to_the_bare_values() {
        let legs = FfiStatusLegs {
            node: Some(FfiNodeLeg {
                domain: "nest.example".into(),
                version: "1.2.3".into(),
            }),
            sync: Some(FfiStatusSyncLeg {
                files_pending: 3,
                bytes_pending: 0,
                last_sync: None,
            }),
            mls: Some(FfiMlsLeg {
                key_packages: 5,
                channels: 2,
            }),
        };
        let build = BuildLeg::from_stamp(Some("0123456789abcdef0123"));
        let text = text_for(&legs, build, 1_000, &Catalog);
        assert_eq!(text.node_domain.as_deref(), Some("nest.example"));
        assert_eq!(text.node_version.as_deref(), Some("1.2.3"));
        assert!(text.sync_pending.unwrap().starts_with("3 files"));
        assert_eq!(text.sync_last.as_deref(), Some("Never"));
        assert_eq!(text.mls_key_packages.as_deref(), Some("5"));
        assert_eq!(text.mls_channels.as_deref(), Some("2"));
        assert_eq!(text.build_sha.as_deref(), Some("0123456789ab"));
    }
}
