//! `fauna.admin.update_channel.*` — which builds this box's updater follows,
//! read and chosen from the admin's app.
//!
//! Owner: `docs/goal/architecture/nest/update-channel.md` § The channel as nest
//! state. The channel travels as its **id string** (`stable` / `test` / `dev`,
//! `fauna_provisioning::update_channel::UpdateChannel::id`), never the enum: this
//! crate sits below `fauna-provisioning`, which owns the enum and the one
//! mapping from a channel to an image tag, and a string lets a channel a newer
//! nest knows reach an older app as an unrecognised value instead of a failed
//! decode — the shape `AdminWebAppOriginGetReply::mode` has.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Value;

/// `fauna.admin.update_channel.get` — no parameters; the answer is about this
/// deployment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AdminUpdateChannelGetRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// What the box follows now and what it runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdminUpdateChannelGetReply {
    /// The channel in force — the admin's choice, else the one the install's
    /// image tag seeded — as its id. Absent on a *pinned* box: one installed
    /// from a tag that is no channel, with no admin choice yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// The running nest's product version (`CARGO_PKG_VERSION`), the same
    /// value `/api/v1/health` serves as `version`.
    pub running_version: String,
    /// The version the channel's tag points at in the registry, when the nest
    /// could resolve it; absent when the registry is unreachable — never a
    /// failed read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_head_version: Option<String>,
    /// Whether a channel can be enacted on this box: `true` on a nest run from
    /// the container image, whose bundle runs the updater; `false` on a nest
    /// the image does not run (a Windows service, a macOS daemon, a Linux
    /// package), where no channel applies, `channel` is absent and `set` is
    /// refused (`nest/update-channel.md` § The channel as nest state → *Where
    /// the channel applies*). Additive (2026-10-19): a nest that predates the
    /// field omits it, and an older nest is an image nest, so absent reads
    /// `true`.
    #[serde(default = "default_enactable")]
    pub enactable: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for AdminUpdateChannelGetReply {
    fn default() -> Self {
        Self {
            channel: None,
            running_version: String::new(),
            channel_head_version: None,
            enactable: true,
            extra: BTreeMap::new(),
        }
    }
}

fn default_enactable() -> bool {
    true
}

/// `fauna.admin.update_channel.set` — a whole-value replace of the admin's
/// choice. There is no "back to the seed" value: once an admin has chosen, a
/// channel is always chosen.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AdminUpdateChannelSetRequest {
    /// The channel's id; one that names no channel this nest knows is refused.
    pub channel: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to `fauna.admin.update_channel.set`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AdminUpdateChannelSetReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_strict, encode_canonical};

    #[test]
    fn a_pinned_read_round_trips_with_the_channel_absent() {
        let reply = AdminUpdateChannelGetReply {
            running_version: "0.1.4".into(),
            ..Default::default()
        };
        let back: AdminUpdateChannelGetReply =
            decode_strict(&encode_canonical(&reply).unwrap()).unwrap();
        assert_eq!(back, reply);
        assert_eq!(back.channel, None);
    }

    #[test]
    fn a_channel_a_newer_nest_knows_decodes_as_a_string() {
        let reply = AdminUpdateChannelGetReply {
            channel: Some("nightly".into()),
            running_version: "0.2.0".into(),
            channel_head_version: Some("0.2.1".into()),
            ..Default::default()
        };
        let back: AdminUpdateChannelGetReply =
            decode_strict(&encode_canonical(&reply).unwrap()).unwrap();
        assert_eq!(back.channel.as_deref(), Some("nightly"));
        assert_eq!(back.channel_head_version.as_deref(), Some("0.2.1"));
    }

    /// A reply from a nest that predates `enactable` carries no such key; it
    /// is an image nest (the only kind that existed), so the field reads
    /// `true` — and a nest the image does not run says `false` on the wire.
    #[test]
    fn an_absent_enactable_reads_true_and_false_survives_the_wire() {
        let older = encode_canonical(&serde_json::json!({ "running_version": "0.1.4" })).unwrap();
        let back: AdminUpdateChannelGetReply = decode_strict(&older).unwrap();
        assert!(back.enactable);
        let packaged = AdminUpdateChannelGetReply {
            running_version: "0.1.5".into(),
            enactable: false,
            ..Default::default()
        };
        let back: AdminUpdateChannelGetReply =
            decode_strict(&encode_canonical(&packaged).unwrap()).unwrap();
        assert!(!back.enactable);
        assert_eq!(back.channel, None);
    }
}
