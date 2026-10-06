//! The preference cluster's **plane write**: the local half of a preference
//! save, and the table of kinds it admits.
//!
//! Authority: `docs/goal/architecture/config-dissolution.md` § The `__config`
//! dissolution schedule (the E1 cluster: the four delegable preference kinds
//! ride the account-state plane, one latest-wins row each at
//! [`PREFERENCE_KEY`]) and `account-client-lifecycle.md` § The client-side
//! lifecycle (the pump bullet → wake source (4): a write through the handle is
//! the local row only).
//!
//! The plane is the cluster's only home. Until closure step (5) of the
//! dissolution schedule this module was the CAS-blob bridge, which also kept
//! the four sub-records mirrored inside the sealed `__config` blob for clients
//! that read them there; the bridge, its blob-side marker and its decision
//! table are deleted, and what is left is the write itself.
//!
//! "Cleared" is a default **value** on this cluster, never a tombstone: a
//! preference page that empties its list puts the default record.

use anyhow::{Context, Result, bail};
use fauna_account_store::{backend::StoreBackend, store::AccountStore};
use fauna_core::data::Timestamp;
use fauna_core::encoding::canonical_decode;
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::{LwwStamp, PREFERENCE_KEY, RecordKind, records};
use serde::de::DeserializeOwned;

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// One admitted preference kind: the plane kind string, and whether a value
/// decodes as the kind's sub-record. The table is the **whole** preference
/// cluster — every other kind has its own typed door.
struct AdmittedKind {
    kind: &'static str,
    decodes: fn(&[u8]) -> bool,
}

/// Whether `bytes` decode as `kind`'s payload — the type comes from the typed
/// kind constant, never beside it.
fn decodes_as<T: DeserializeOwned>(_kind: &RecordKind<T>, bytes: &[u8]) -> bool {
    canonical_decode::<T>(bytes).is_ok()
}

const ADMITTED: &[AdmittedKind] = &[
    AdmittedKind {
        kind: records::MODERATION.name,
        decodes: |b| decodes_as(&records::MODERATION, b),
    },
    AdmittedKind {
        kind: records::SYNC_PREFS.name,
        decodes: |b| decodes_as(&records::SYNC_PREFS, b),
    },
    AdmittedKind {
        kind: records::PERSONALIZATION.name,
        decodes: |b| decodes_as(&records::PERSONALIZATION, b),
    },
    AdmittedKind {
        kind: records::DELEGATION.name,
        decodes: |b| decodes_as(&records::DELEGATION, b),
    },
];

fn admitted(kind: &str) -> Option<&'static AdmittedKind> {
    ADMITTED.iter().find(|k| k.kind == kind)
}

/// Is `kind` one of the four preference records — the kinds the delegable
/// publish retries first among its parked rows (`account-replica-posture.md`
/// § The store device principal, refinement 11 → *A row refused for room is
/// parked*)?
pub fn is_preference_kind(kind: &str) -> bool {
    admitted(kind).is_some()
}

/// Plane stamps are milliseconds ([`LwwStamp::at_ms`]); [`Timestamp::now`] is
/// **microseconds**.
fn micros_to_ms(t: Timestamp) -> i64 {
    (t.0 / 1_000) as i64
}

/// The production preference write's **local half** — the only half a
/// command serves (`account-client-lifecycle.md` § The client-side lifecycle,
/// the pump bullet → wake source (4)).
///
/// `value` is the sub-record's canonical dag-cbor bytes. The plane row lands
/// here, durable and stamped, and **nothing is sent**: the network leg — the
/// plane's ordered own publish ([`AccountStatePlane::publish_pending`]) — is
/// the account runtime's publish step, armed by this write and run as soon as
/// no pass is in flight. So a preference save answers while a pass runs and
/// never reports a network failure as its own.
///
/// Returns the stamp the row carries — the one it will publish under.
pub async fn put_preference_local<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    kind: &str,
    value: Vec<u8>,
) -> Result<LwwStamp>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let Some(k) = admitted(kind) else {
        bail!(
            "put_preference: {kind:?} is not an admitted preference kind — this door covers \
             exactly the delegable preference cluster (see the ADMITTED table)"
        );
    };
    // Validate before the row exists: a value that is not the kind's
    // sub-record must not become a plane entry other replicas adopt.
    if !(k.decodes)(&value) {
        bail!("put_preference: value for {kind:?} does not decode as its sub-record");
    }
    let stamp = LwwStamp {
        at_ms: micros_to_ms(Timestamp::now()),
        writer: store.writer().0,
    };
    plane
        .put_local(
            &ItemId {
                kind: kind.to_string(),
                key: PREFERENCE_KEY.to_string(),
            },
            value,
            Some(stamp.encode()?),
        )
        .await
        .context("put_preference: plane put")?;
    Ok(stamp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::ModerationConfig;
    use fauna_core::encoding::canonical_encode;
    use fauna_protocol::merge_policy::{
        KIND_DELEGATION, KIND_MODERATION, KIND_PERSONALIZATION, KIND_SYNC_PREFS, MergePolicy,
        audience_rung, merge_policy,
    };

    /// This door's assumptions about its kinds are registry facts, not table
    /// hopes: every admitted kind is registered delegable latest-wins (the
    /// stamp [`put_preference_local`] mints is what its merge reads), and the
    /// table covers exactly the charter's preference cluster.
    #[test]
    fn the_admitted_table_matches_the_registry() {
        let kinds: Vec<_> = ADMITTED.iter().map(|k| k.kind).collect();
        assert_eq!(
            kinds,
            vec![
                KIND_MODERATION,
                KIND_SYNC_PREFS,
                KIND_PERSONALIZATION,
                KIND_DELEGATION
            ]
        );
        for k in kinds {
            assert_eq!(merge_policy(k), Some(MergePolicy::LatestWins), "{k}");
            assert_eq!(
                audience_rung(k),
                Some(fauna_core::crypto::AudienceRung::Delegable),
                "{k}"
            );
        }
    }

    /// A value is admitted only as its own kind's sub-record.
    #[test]
    fn a_value_is_admitted_only_as_its_kinds_sub_record() {
        let moderation = canonical_encode(&ModerationConfig {
            muted_keywords: vec!["lottery".into()],
            ..Default::default()
        })
        .unwrap();
        let k = admitted(KIND_MODERATION).expect("admitted");
        assert!((k.decodes)(&moderation));
        assert!(!(k.decodes)(b"not a record"));
        assert!(admitted("fauna.state.dns").is_none());
    }
}
