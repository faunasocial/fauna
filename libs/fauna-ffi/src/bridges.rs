//! UniFFI façade for the `fauna.bridges.*` Layer-3 Bridge Management
//! WS-RPC kinds — list / link / unlink / settings / follows / feeds, the
//! surface clients drive from the Bridges page.
//!
//! [`FfiBridgesClient`] wraps `fauna_client_bridges::BridgesClient`; the
//! mirror records below are the FFI-visible shape of
//! `fauna_protocol::bridges_ui::*`. Dynamic per-bridge values (a setting's
//! value, a follow's `extra` blob) cross as [`FfiCborValue`] so the wire
//! stays self-describing without a client-side CBOR codec. The
//! Rust-native Linux app (`apps/fauna-linux/src/views/bridges/`) calls
//! the same `BridgesClient` directly.
//!
//! Protocol → FFI conversions are `TryFrom` (a non-text CBOR map key or an
//! out-of-`i64` integer in a setting value is a typed error). FFI → protocol
//! conversions are exhaustive matches, so a new field on a `bridges_ui`
//! type is a compile error here (mirror-drift guard, priority #1/#4).

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_bridges::BridgesClient;
use fauna_client_bridges::bridges_ui::{
    BridgeFollow, BridgeIdentity, BridgeLinkField, BridgeLinkMode, BridgeSetting,
    BridgeSettingOption, BridgeStatus, FeedSubscription, LinkReply,
};

use crate::cbor::FfiCborValue;
use crate::{FfiError, stringify};

/// UniFFI face of [`fauna_client_bridges::nostr_key_source_label`] — the shared
/// Nostr signing-mode → label map (`generated`/`imported`/`remote`/`nip07` →
/// `nostr.account.mode_*`, an unknown/not-linked value verbatim), returned as a
/// [`LocalizedText`](fauna_core::localized::LocalizedText) each app resolves
/// through its own i18n runtime. Lifts the per-app raw-enum `Text(mode)`
/// renders onto one source of truth so every app with the Nostr page shows the
/// same label (priority #2/#4); the Signing Mode row (`NostrSettingsView`)
/// renders it. See `docs/goal/ui/nostr.md` § Signing-mode display.
///
/// Gated behind `value-format` for the SAME reason as `reminder_label` /
/// `rsvp_status_label` (caldav_client.rs): a bare `fauna_core::LocalizedText`
/// crosses the boundary, which `uniffi-bindgen-go` emits as an uncompilable
/// cross-namespace import in the Go mail-bridge's `--no-default-features` build
/// (no Nostr UI there, so dropping it is harmless → no `mail-bridge-ffi` regen).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn nostr_key_source_label(mode: String) -> fauna_core::localized::LocalizedText {
    fauna_client_bridges::nostr_key_source_label(&mode)
}

/// UniFFI face of [`fauna_client_bridges::nostr_link_mode_label`] — the
/// Nostr link-*request*-mode (`generate`/`import`/`remote`/`nip07`) → label
/// map, the request-mode twin of [`nostr_key_source_label`] above. Android /
/// windows / apple each hand-write their own `generate`/`import`/`remote` →
/// label match for the `nostr-link-mode` picker (`docs/goal/ui/nostr.md` §
/// Account linking); this is the shared source they can call instead. Linux
/// and tui call `fauna_client_bridges::nostr_link_mode_label` directly
/// (native Rust, no FFI hop).
///
/// Gated behind `value-format` for the SAME reason as
/// [`nostr_key_source_label`].
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn nostr_link_mode_label(mode: String) -> fauna_core::localized::LocalizedText {
    fauna_client_bridges::nostr_link_mode_label(&mode)
}

/// UniFFI face of [`fauna_client_bridges::nostr_content_toggle_options`] — the
/// five Nostr content-publishing toggles as one table
/// (`docs/goal/ui/nostr.md` § Where logic lives), each row carrying its wire
/// settings key, its `ui.yaml` element id, the nest-matching default, and its
/// title + optional subtitle. Android / apple / windows render straight off
/// this instead of each hand-writing the same five rows; linux and tui call
/// `fauna_client_bridges::nostr_content_toggle_options` directly (native Rust,
/// no FFI hop), and web has the `nostrContentToggleOptions` wasm twin.
///
/// Gated behind `value-format` for the SAME reason as
/// [`nostr_key_source_label`]: the rows carry
/// [`LocalizedText`](fauna_core::localized::LocalizedText), which
/// `uniffi-bindgen-go` emits as an uncompilable cross-namespace import in the
/// Go mail-bridge's `--no-default-features` build. The bridge renders no Nostr
/// settings screen, so dropping it there is harmless → no `mail-bridge-ffi`
/// regen.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn nostr_content_toggle_options() -> Vec<FfiBridgeToggleOption> {
    fauna_client_bridges::nostr_content_toggle_options()
        .into_iter()
        .map(FfiBridgeToggleOption::from)
        .collect()
}

/// UniFFI face of [`fauna_client_bridges::is_unified_bridges_page_bridge`] —
/// whether a `fauna.bridges.list` row belongs on the unified Bridges page
/// (`bridges.md` § Scope; Nostr has its own dedicated page). A plain `bool`,
/// so no `value-format` gating concern.
#[uniffi::export]
pub fn is_unified_bridges_page_bridge(id: String) -> bool {
    fauna_client_bridges::is_unified_bridges_page_bridge(&id)
}

/// UniFFI face of [`fauna_client_bridges::LinkBlock`] — why a bridge's Link
/// control is not actionable.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum FfiLinkBlock {
    /// The nest's own explanation of a `provider.status()` failure. Render
    /// `message` verbatim — never swap in a localized generic string.
    ProviderError { message: String },
    /// No declared mode applies on this platform; render the localized
    /// `bridges.no_link_method`.
    NoApplicableMode,
}

/// UniFFI face of [`fauna_client_bridges::follow_display`] — a follow's
/// display name: the petname if set and non-blank, else the raw external id
/// (the Nostr `follow_display` convention already shared by linux and tui).
/// Takes the two fields the rule reads rather than a full [`FfiBridgeFollow`]
/// — same predicate-shaped rationale as [`bridge_mode_applies`] below: no
/// caller needs to build the FFI mirror record just to compute a label.
#[uniffi::export]
pub fn follow_display(id: String, petname: Option<String>) -> String {
    fauna_client_bridges::follow_display(&BridgeFollow {
        id,
        petname,
        created_at: None,
        extra: None,
        unknown_keys: Default::default(),
    })
}

/// UniFFI face of [`fauna_client_bridges::mode_applies`] — does one declared
/// link mode apply on `platform`? A mode with no `platform` is universal; a
/// scoped one applies only on the app whose **canonical name** it names
/// (`linux`/`windows`/`macos`/`ios`/`android`/`web`/`tui` — the ui.yaml
/// vocabulary; the shared module's docs own the rule and the retired
/// `"desktop"` alias's story).
///
/// Predicate-shaped on purpose: every native shell decodes bridges into its
/// OWN model types (windows' C# `BridgeInfo`, apple's FaunaKit structs), so a
/// batch filter over [`FfiBridgeLinkMode`] would force each caller to map its
/// list into the FFI record and back just to drop an element. A per-mode
/// `bool` needs no mapping — the caller's filter keeps its list type and swaps
/// only the condition, which is exactly the piece that was hand-written seven
/// times. Runtime capability checks (web's nip07 extension probe) stay
/// caller-local, ANDed after this.
#[uniffi::export]
pub fn bridge_mode_applies(mode_platform: Option<String>, platform: String) -> bool {
    fauna_client_bridges::mode_applies(mode_platform.as_deref(), &platform)
}

/// UniFFI face of [`fauna_client_bridges::link_block`] — `None` means the Link
/// control is actionable (`bridges.md` § Errors & edge cases).
///
/// Takes the two `BridgeStatus` fields the rule actually reads rather than the
/// whole [`FfiBridgeStatus`]: UniFFI records cross by value, so passing the full
/// row (with its settings and modes vectors) would copy the entire bridge on
/// every render just to look at two fields. `applicable_modes` is the count
/// *after* the caller's own platform filter — the string-match half of which is
/// [`bridge_mode_applies`] above; only platform identity and runtime capability
/// checks stay local.
#[uniffi::export]
pub fn bridge_link_block(
    linked: bool,
    error: Option<String>,
    applicable_modes: u32,
) -> Option<FfiLinkBlock> {
    fauna_client_bridges::link_block_of(linked, error.as_deref(), applicable_modes as usize).map(
        |b| match b {
            fauna_client_bridges::LinkBlock::ProviderError(why) => FfiLinkBlock::ProviderError {
                message: why.to_string(),
            },
            fauna_client_bridges::LinkBlock::NoApplicableMode => FfiLinkBlock::NoApplicableMode,
        },
    )
}

// ── Record mirrors ─────────────────────────────────────────────────────

/// FFI mirror of [`fauna_client_bridges::BridgeToggleOption`] — one row of a
/// bridge-settings boolean-toggle catalog. `value-format`-gated with its
/// producer above, both because it carries `LocalizedText` and because nothing
/// else references it.
#[cfg(feature = "value-format")]
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiBridgeToggleOption {
    pub key: String,
    pub ui_id: String,
    /// Spelled `default_on`, not `default`, all the way from the shared type —
    /// `default` is a keyword in both C# and Swift, so the bare name would
    /// reach those two apps escaped rather than as written. See
    /// [`fauna_client_bridges::BridgeToggleOption::default_on`].
    pub default_on: bool,
    pub label: fauna_core::localized::LocalizedText,
    pub subtitle: Option<fauna_core::localized::LocalizedText>,
}

#[cfg(feature = "value-format")]
impl From<fauna_client_bridges::BridgeToggleOption> for FfiBridgeToggleOption {
    fn from(o: fauna_client_bridges::BridgeToggleOption) -> Self {
        FfiBridgeToggleOption {
            key: o.key,
            ui_id: o.ui_id,
            default_on: o.default_on,
            label: o.label,
            subtitle: o.subtitle,
        }
    }
}

/// FFI mirror of [`fauna_protocol::bridges_ui::BridgeIdentity`].
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiBridgeIdentity {
    pub label: String,
    pub value: String,
    pub display: String,
}

impl From<BridgeIdentity> for FfiBridgeIdentity {
    fn from(i: BridgeIdentity) -> Self {
        FfiBridgeIdentity {
            label: i.label,
            value: i.value,
            display: i.display,
        }
    }
}

/// FFI mirror of [`fauna_protocol::bridges_ui::BridgeSettingOption`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiBridgeSettingOption {
    pub value: FfiCborValue,
    pub label: String,
}

impl TryFrom<BridgeSettingOption> for FfiBridgeSettingOption {
    type Error = FfiError;

    fn try_from(o: BridgeSettingOption) -> Result<Self, FfiError> {
        Ok(FfiBridgeSettingOption {
            value: FfiCborValue::try_from(o.value)?,
            label: o.label,
        })
    }
}

/// FFI mirror of [`fauna_protocol::bridges_ui::BridgeSetting`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiBridgeSetting {
    pub key: String,
    pub label: String,
    pub setting_type: String,
    pub value: FfiCborValue,
    pub options: Option<Vec<FfiBridgeSettingOption>>,
}

impl TryFrom<BridgeSetting> for FfiBridgeSetting {
    type Error = FfiError;

    fn try_from(s: BridgeSetting) -> Result<Self, FfiError> {
        let options = match s.options {
            Some(opts) => Some(
                opts.into_iter()
                    .map(FfiBridgeSettingOption::try_from)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            None => None,
        };
        Ok(FfiBridgeSetting {
            key: s.key,
            label: s.label,
            setting_type: s.setting_type,
            value: FfiCborValue::try_from(s.value)?,
            options,
        })
    }
}

/// FFI mirror of [`fauna_protocol::bridges_ui::BridgeLinkField`].
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiBridgeLinkField {
    pub key: String,
    pub label: String,
    pub field_type: String,
    pub placeholder: Option<String>,
}

impl From<BridgeLinkField> for FfiBridgeLinkField {
    fn from(f: BridgeLinkField) -> Self {
        FfiBridgeLinkField {
            key: f.key,
            label: f.label,
            field_type: f.field_type,
            placeholder: f.placeholder,
        }
    }
}

/// FFI mirror of [`fauna_protocol::bridges_ui::BridgeLinkMode`].
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiBridgeLinkMode {
    pub mode: String,
    pub label: String,
    pub client_action: Option<String>,
    pub platform: Option<String>,
    pub fields: Vec<FfiBridgeLinkField>,
}

impl From<BridgeLinkMode> for FfiBridgeLinkMode {
    fn from(m: BridgeLinkMode) -> Self {
        FfiBridgeLinkMode {
            mode: m.mode,
            label: m.label,
            client_action: m.client_action,
            platform: m.platform,
            fields: m.fields.into_iter().map(Into::into).collect(),
        }
    }
}

/// FFI mirror of [`fauna_protocol::bridges_ui::BridgeStatus`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiBridgeStatus {
    pub id: String,
    pub name: String,
    pub available: bool,
    pub linked: bool,
    pub identity: Option<FfiBridgeIdentity>,
    pub mode: Option<String>,
    pub settings: Vec<FfiBridgeSetting>,
    pub supports_follows: bool,
    pub link_modes: Option<Vec<FfiBridgeLinkMode>>,
    pub error: Option<String>,
}

impl TryFrom<BridgeStatus> for FfiBridgeStatus {
    type Error = FfiError;

    fn try_from(s: BridgeStatus) -> Result<Self, FfiError> {
        Ok(FfiBridgeStatus {
            id: s.id,
            name: s.name,
            available: s.available,
            linked: s.linked,
            identity: s.identity.map(Into::into),
            mode: s.mode,
            settings: s
                .settings
                .into_iter()
                .map(FfiBridgeSetting::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            supports_follows: s.supports_follows,
            link_modes: s
                .link_modes
                .map(|modes| modes.into_iter().map(Into::into).collect()),
            error: s.error,
        })
    }
}

/// FFI mirror of [`fauna_protocol::bridges_ui::LinkReply`].
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiLinkReply {
    pub linked: bool,
    pub identity: Option<FfiBridgeIdentity>,
    pub redirect_url: Option<String>,
}

impl From<LinkReply> for FfiLinkReply {
    fn from(r: LinkReply) -> Self {
        FfiLinkReply {
            linked: r.linked,
            identity: r.identity.map(Into::into),
            redirect_url: r.redirect_url,
        }
    }
}

/// FFI mirror of [`fauna_protocol::bridges_ui::BridgeFollow`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiBridgeFollow {
    pub id: String,
    pub petname: Option<String>,
    pub created_at: Option<i64>,
    pub extra: Option<FfiCborValue>,
}

impl TryFrom<BridgeFollow> for FfiBridgeFollow {
    type Error = FfiError;

    fn try_from(f: BridgeFollow) -> Result<Self, FfiError> {
        Ok(FfiBridgeFollow {
            id: f.id,
            petname: f.petname,
            created_at: f.created_at,
            extra: f.extra.map(FfiCborValue::try_from).transpose()?,
        })
    }
}

/// FFI mirror of [`fauna_protocol::bridges_ui::FeedSubscription`].
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFeedSubscription {
    pub id: i64,
    pub bridge: String,
    pub feed_uri: String,
    pub name: String,
    pub created_at: i64,
}

impl From<FeedSubscription> for FfiFeedSubscription {
    fn from(s: FeedSubscription) -> Self {
        FfiFeedSubscription {
            id: s.id,
            bridge: s.bridge,
            feed_uri: s.feed_uri,
            name: s.name,
            created_at: s.created_at,
        }
    }
}

// ── FfiBridgesClient ───────────────────────────────────────────────────

/// UniFFI handle for the `fauna.bridges.*` kinds. Construct via
/// [`crate::nest_client::FfiNestClient::bridges`]; methods are exposed to
/// Swift as `async throws` and Kotlin as `suspend fun`.
#[derive(uniffi::Object)]
pub struct FfiBridgesClient {
    nest: Arc<NestClient>,
}

impl FfiBridgesClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> BridgesClient<Arc<NestClient>> {
        BridgesClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiBridgesClient {
    /// `fauna.bridges.list`
    pub async fn list(&self) -> Result<Vec<FfiBridgeStatus>, FfiError> {
        let reply = self.client().list().await.map_err(stringify)?;
        reply
            .bridges
            .into_iter()
            .map(FfiBridgeStatus::try_from)
            .collect()
    }

    /// `fauna.bridges.link` — `forbid_replay` at 30 s. `params` carries the
    /// per-mode field values (see the link-mode metadata in
    /// [`FfiBridgeStatus::link_modes`]).
    pub async fn link(
        &self,
        bridge_id: String,
        mode: String,
        params: FfiCborValue,
    ) -> Result<FfiLinkReply, FfiError> {
        let reply = self
            .client()
            .link(bridge_id, mode, params.into())
            .await
            .map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.bridges.unlink`
    pub async fn unlink(&self, bridge_id: String) -> Result<(), FfiError> {
        self.client().unlink(bridge_id).await.map_err(stringify)
    }

    /// `fauna.bridges.set_settings`
    pub async fn set_settings(
        &self,
        bridge_id: String,
        settings: FfiCborValue,
    ) -> Result<(), FfiError> {
        self.client()
            .set_settings(bridge_id, settings.into())
            .await
            .map_err(stringify)
    }

    /// `fauna.bridges.list_follows`
    pub async fn list_follows(&self, bridge_id: String) -> Result<Vec<FfiBridgeFollow>, FfiError> {
        let reply = self
            .client()
            .list_follows(bridge_id)
            .await
            .map_err(stringify)?;
        reply
            .follows
            .into_iter()
            .map(FfiBridgeFollow::try_from)
            .collect()
    }

    /// `fauna.bridges.add_follow` — `forbid_replay`.
    pub async fn add_follow(
        &self,
        bridge_id: String,
        id: String,
        petname: Option<String>,
        extra: Option<FfiCborValue>,
    ) -> Result<(), FfiError> {
        self.client()
            .add_follow(bridge_id, id, petname, extra.map(Into::into))
            .await
            .map_err(stringify)
    }

    /// `fauna.bridges.remove_follow`
    pub async fn remove_follow(
        &self,
        bridge_id: String,
        follow_id: String,
    ) -> Result<(), FfiError> {
        self.client()
            .remove_follow(bridge_id, follow_id)
            .await
            .map_err(stringify)
    }

    /// `fauna.bridges.feeds.list`
    pub async fn feeds_list(&self) -> Result<Vec<FfiFeedSubscription>, FfiError> {
        let reply = self.client().feeds_list().await.map_err(stringify)?;
        Ok(reply.subscriptions.into_iter().map(Into::into).collect())
    }

    /// `fauna.bridges.feeds.create` — returns the new (or existing) row id.
    pub async fn feeds_create(
        &self,
        bridge: String,
        feed_uri: String,
        name: String,
    ) -> Result<i64, FfiError> {
        self.client()
            .feeds_create(bridge, feed_uri, name)
            .await
            .map_err(stringify)
    }

    /// `fauna.bridges.feeds.delete`
    pub async fn feeds_delete(&self, id: i64) -> Result<(), FfiError> {
        self.client().feeds_delete(id).await.map_err(stringify)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client::Value;
    use fauna_client_bridges::bridges_ui::BridgeSettingOption as ProtoOpt;
    use std::collections::BTreeMap;

    #[test]
    fn bridge_status_with_cbor_setting_maps() {
        let proto = BridgeStatus {
            id: "bluesky".into(),
            name: "Bluesky".into(),
            available: true,
            linked: true,
            identity: Some(BridgeIdentity {
                label: "Handle".into(),
                value: "did:plc:abc".into(),
                display: "alice.bsky.social".into(),
                extra: Default::default(),
            }),
            mode: Some("personal".into()),
            settings: vec![BridgeSetting {
                key: "write_through".into(),
                label: "Crosspost".into(),
                setting_type: "enum".into(),
                value: Value::Integer(1),
                options: Some(vec![
                    ProtoOpt {
                        value: Value::Integer(0),
                        label: "Off".into(),
                        extra: Default::default(),
                    },
                    ProtoOpt {
                        value: Value::Integer(1),
                        label: "On".into(),
                        extra: Default::default(),
                    },
                ]),
                extra: Default::default(),
            }],
            supports_follows: true,
            supports_follow_requests: false,
            link_modes: None,
            glyph: None,
            error: None,
            extra: Default::default(),
        };
        let ffi = FfiBridgeStatus::try_from(proto).unwrap();
        assert_eq!(ffi.id, "bluesky");
        assert_eq!(ffi.settings.len(), 1);
        assert_eq!(ffi.settings[0].value, FfiCborValue::Integer { v: 1 });
        assert_eq!(ffi.settings[0].options.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn follow_display_prefers_a_non_blank_petname_over_the_raw_id() {
        assert_eq!(
            follow_display("did:plc:abc".into(), Some("Alice".into())),
            "Alice"
        );
        assert_eq!(follow_display("did:plc:abc".into(), None), "did:plc:abc");
        // A whitespace-only petname is treated as unset, not as a blank label
        // — the exact edge case Android's hand-rolled `petname ?: id` missed.
        assert_eq!(
            follow_display("did:plc:abc".into(), Some("   ".into())),
            "did:plc:abc"
        );
    }

    #[test]
    fn follow_with_extra_maps() {
        let proto = BridgeFollow {
            id: "did:plc:abc".into(),
            petname: Some("Alice".into()),
            created_at: Some(1_700_000_000),
            extra: Some(Value::Map(BTreeMap::from([(
                "handle".to_string(),
                Value::String("alice.bsky.social".into()),
            )]))),
            unknown_keys: Default::default(),
        };
        let ffi = FfiBridgeFollow::try_from(proto).unwrap();
        assert_eq!(ffi.id, "did:plc:abc");
        assert!(matches!(ffi.extra, Some(FfiCborValue::Map { .. })));
    }

    #[test]
    fn link_reply_maps() {
        let proto = LinkReply {
            linked: false,
            identity: None,
            redirect_url: Some("https://bsky.social/oauth".into()),
            extra: Default::default(),
        };
        let ffi: FfiLinkReply = proto.into();
        assert!(!ffi.linked);
        assert_eq!(
            ffi.redirect_url.as_deref(),
            Some("https://bsky.social/oauth")
        );
    }

    #[test]
    fn link_params_compose_outbound() {
        // The shape the Kotlin/Swift dialog builds for `link`.
        let params = FfiCborValue::Map {
            entries: vec![crate::cbor::FfiCborEntry {
                key: "handle".into(),
                value: FfiCborValue::Text {
                    v: "alice.bsky.social".into(),
                },
            }],
        };
        let cbor: Value = params.into();
        assert!(matches!(cbor, Value::Map(_)));
    }
}
