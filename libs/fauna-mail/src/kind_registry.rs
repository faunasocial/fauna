//! Pass-through to `fauna-protocol::KindRegistry` for use over UniFFI.
//!
//! Bridges (and other clients) need to look up per-kind metadata (deadlines,
//! forbid_replay flags) when composing WS-RPC frames. The registry itself
//! lives in fauna-protocol; this module exposes the lookup function over
//! UniFFI without duplicating the registry data.

use fauna_protocol::kind::{KindRegistry, RpcKindMeta};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KindMetadata {
    pub forbid_replay: bool,
    pub default_deadline_ms: u64,
}

impl From<RpcKindMeta> for KindMetadata {
    fn from(m: RpcKindMeta) -> Self {
        Self {
            forbid_replay: m.forbid_replay,
            default_deadline_ms: m.default_deadline.as_millis() as u64,
        }
    }
}

/// Look up a kind's metadata in the production protocol registry.
/// Returns `None` if the kind is not registered.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn lookup_kind(kind: &str) -> Option<KindMetadata> {
    let registry = KindRegistry::full();
    registry.meta(kind).map(KindMetadata::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_known_kind_returns_metadata() {
        let m = lookup_kind("fauna.protocol.echo").expect("echo kind should be registered");
        assert!(!m.forbid_replay);
        assert_eq!(m.default_deadline_ms, 5_000);
    }

    #[test]
    fn lookup_unknown_kind_returns_none() {
        assert!(lookup_kind("does.not.exist").is_none());
    }
}
