//! The owner-fleet side's NEST-form custodians — the reader the custodian
//! restore walk ([`crate::custodian_restore`]) dials from
//! (`account-replica-posture.md` § Replica posture → *The custody grant +
//! ceremony*, the custodian-NEST serve door bullet, ruling 6).
//!
//! A `fauna.state.custodian-endpoints` row carrying `custodian_nest_url` is no
//! peer target: it has zero candidates and the nest runs no peer endpoint, so
//! it is dialed over the nest wire. The DEVICE-form complement stays the peer
//! plane's (`fauna_peer_sync::custodian_dial_targets`), with its own decode
//! loop over the same rows — `fauna-peer-sync` cannot depend on this crate
//! (`fauna-client-capabilities` depends on it natively, and this crate on
//! `fauna-client-capabilities`, so that edge would close a cycle).

use anyhow::Result;
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_core::custodian_endpoints::CustodianEndpoints;
use fauna_core::encoding::canonical_decode;

/// One NEST-form custodian the owner's fleet may restore from: the custody
/// grant it holds (checked against the dialer's revocation view before any
/// dial) and the URL the host's accept bound to its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodianNestTarget {
    pub grant_id: Vec<u8>,
    /// The counterparty's string, as the row carries it — unvalidated here;
    /// the dial site applies the counterparty URL policy.
    pub url: String,
    /// The host's pinned nest actor identity the accept bound
    /// (`endpoints.node_id` of a NEST-form row) — what the restore gesture
    /// checks the answering connection against after its walk
    /// (`account-replica-posture.md` § *Corollary — the cold device*, bound (c)).
    pub custodian_key: [u8; 32],
}

/// One target per `fauna.state.custodian-endpoints` row carrying
/// `custodian_nest_url`. An undecodable row is skipped, never fatal — the
/// honest rows beside it stay targets (canonical-or-skip, as every
/// location-data reader).
pub async fn custodian_nest_targets<B: StoreBackend>(
    store: &AccountStore<B>,
) -> Result<Vec<CustodianNestTarget>> {
    let mut targets = Vec::new();
    for entry in store
        .states_of_kind(fauna_protocol::merge_policy::KIND_CUSTODIAN_ENDPOINTS)
        .await?
    {
        match canonical_decode::<CustodianEndpoints>(&entry.value) {
            Ok(value) => {
                if let Some(url) = value.custodian_nest_url {
                    targets.push(CustodianNestTarget {
                        grant_id: value.grant_id,
                        url,
                        custodian_key: value.endpoints.node_id,
                    });
                }
            }
            Err(_) => {
                tracing::warn!(key = %entry.key, "undecodable custodian-endpoints entry — skipped")
            }
        }
    }
    Ok(targets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_account_store::types::{StateEntry, WriterId};
    use fauna_core::encoding::canonical_encode;
    use fauna_protocol::account_state::ACCOUNT_STATE_SCOPE;
    use fauna_protocol::merge_policy::KIND_CUSTODIAN_ENDPOINTS;

    async fn put(store: &AccountStore<SqliteBackend>, key: String, value: Vec<u8>) {
        // Door-less staging (the sanctioned conformance pattern).
        store
            .put_state(StateEntry {
                kind: KIND_CUSTODIAN_ENDPOINTS.to_string(),
                key,
                scope: ACCOUNT_STATE_SCOPE.to_string(),
                value,
                merge_meta: None,
                entry_version: 0,
                tombstone: false,
            })
            .await
            .unwrap();
    }

    /// Only NEST-form rows are nest targets, and an undecodable row beside
    /// them is skipped: the honest NEST-form row stays a target.
    #[tokio::test]
    async fn nest_form_rows_are_targets_and_a_bad_row_is_skipped() {
        let store = AccountStore::open(
            SqliteBackend::open_in_memory().unwrap(),
            "aa11",
            WriterId([0x11; 32]),
        )
        .await
        .unwrap();
        let mut device_form = CustodianEndpoints {
            grant_id: vec![0xB0; 16],
            ..Default::default()
        };
        device_form.endpoints.node_id = [0xB1; 32];
        let mut nest_form = CustodianEndpoints {
            grant_id: vec![0xD0; 16],
            custodian_nest_url: Some("https://custodian.example/".into()),
            ..Default::default()
        };
        nest_form.endpoints.node_id = [0xC5; 32];
        for row in [&device_form, &nest_form] {
            put(
                &store,
                fauna_core::custody_grant::custody_entry_key(&row.grant_id),
                canonical_encode(row).unwrap(),
            )
            .await;
        }
        put(&store, "c0ffee".into(), vec![0xFF, 0x00, 0x13]).await;

        assert_eq!(
            custodian_nest_targets(&store).await.unwrap(),
            vec![CustodianNestTarget {
                grant_id: nest_form.grant_id.clone(),
                url: "https://custodian.example/".into(),
                custodian_key: [0xC5; 32],
            }]
        );
    }
}
