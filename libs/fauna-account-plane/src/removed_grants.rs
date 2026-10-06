//! **The removal's nest half follows merged state** — at every nest a runtime
//! completes, the grant of every roster row whose principal merged state reads
//! `Removed` is revoked, by key.
//!
//! Authority: `docs/goal/architecture/account-data-taxonomy.md` § The
//! generation machinery → *Fleet-scope reclamation*, clause (4) → *The nest
//! half follows merged state*; the secondary leg's step is
//! `account-sync-plane.md` § The bind leg, ruling 8.
//!
//! A removal's nest-side deletion reaches only the nest the removing app is
//! bound to, and a removal by key (the member-addressed door) sends nothing
//! nest-side at all. So a device bound to another of the account's nests kept
//! its grant there: that nest went on minting for a key the user removed, and
//! its retention gate went on counting the removed device's walk mark. This
//! module is the one function that closes it, called from the two places a
//! runtime completes a nest:
//!
//! - **the bound nest**, in every full pass (`account_driver::pass::pump`), on
//!   the account's session — whichever process pumps, the seedless agent
//!   included: the arm needs no seed;
//! - **each linked replica**, in every secondary-leg run
//!   (`linked_leg::complete_linked_nest`), after that nest's reconcile and
//!   ahead of its retires, so the run that revokes is the run whose retires
//!   land.
//!
//! # The rule
//!
//! 1. Nothing is asked of the nest until merged state reads some **other**
//!    device removed ([`removed_others`]). This device's own key is never
//!    named: a removed device's own ending is
//!    `account-replica-posture.md`'s.
//! 2. Then the nest's roster is read (`fauna.sync.devices.list`), once per
//!    nest per run.
//! 3. For every row whose `principal` is a removed id, the grant is revoked
//!    **by that key** (`fauna.sync.device_grant.revoke`, the app/user arm: no
//!    proof of possession). The key is client-held truth — a fleet id read
//!    from a `Removed` row this replica verified — and `Removed` is absorbing,
//!    so revoking it is right wherever it rests and can never reach a device
//!    that is still a member. The roster decides only whether to ask.
//! 4. **The row is kept.** Nothing here sends `fauna.sync.devices.delete`: a
//!    deletion is addressed by row, and a machine that signs out and in again
//!    enrolls a fresh principal on the same named row, so a row-addressed
//!    request can end a live successor. A key-addressed one cannot.
//! 5. **A guardian-marked row is left alone** — skipped when the roster flags
//!    it, and a `fauna.sync.guardian_marked` refusal from the nest (an older
//!    listing, a race with the mark) is counted, never an error
//!    (`family-safety.md` § Full visibility for young children → *The device
//!    marker*).
//!
//! A landed revoke clears the row's grant columns, so the row no longer lists
//! the principal and later runs send nothing for it: the steady-state cost is
//! the one roster read.

use std::collections::{BTreeSet, HashSet};

use anyhow::Result;
use fauna_account_store::{backend::StoreBackend, store::AccountStore};
use fauna_protocol::sync::{
    DeviceGrantRevokeReply, DeviceGrantRevokeRequest, SyncDevice, SyncDevicesListReply,
    SyncDevicesListRequest,
};
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::generation_tip::GenerationTrust;

/// The WS-RPC kind a nest's device roster is read with.
pub const KIND_DEVICES_LIST: &str = "fauna.sync.devices.list";

/// The WS-RPC kind one grant is revoked with, named by its device key.
pub const KIND_DEVICE_GRANT_REVOKE: &str = "fauna.sync.device_grant.revoke";

/// The nest's typed refusal of the app/user arm for a guardian-marked row's
/// key (`bins/fauna-nest/src/sync_handlers.rs::device_grant_revoke_handler`).
pub const CODE_GUARDIAN_MARKED: &str = "fauna.sync.guardian_marked";

/// What one run did at one nest (the pump's `removed_grants` slot, and
/// `LinkedCompletion::removed_grants`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemovedGrantsPass {
    /// The nest's roster was read. `false` on every run before the account's
    /// first removal — nothing was asked of the nest — and on a run whose
    /// roster read failed (then [`Self::errors`] says why).
    pub roster_read: bool,
    /// Grants the nest took the revoke of.
    pub revoked: usize,
    /// Rows claiming a removed principal that the roster flags
    /// guardian-marked: nothing was sent for them.
    pub skipped_marked: usize,
    /// Revokes the nest refused `fauna.sync.guardian_marked`.
    pub refused_marked: usize,
    /// What failed — the roster read, or a revoke. The next run asks again.
    pub errors: Vec<String>,
}

impl RemovedGrantsPass {
    /// A run whose roster read failed: nothing was sent.
    #[must_use]
    pub fn unread(why: impl std::fmt::Display) -> Self {
        Self {
            errors: vec![format!("{KIND_DEVICES_LIST}: {why}")],
            ..Self::default()
        }
    }
}

/// The fleet ids merged state reads `Removed`, minus this device's own
/// (`me`) — the keys the arm may name.
///
/// # Errors
///
/// Store I/O.
pub async fn removed_others<B: StoreBackend>(
    store: &AccountStore<B>,
    trust: &GenerationTrust,
    me: &[u8; 32],
) -> Result<HashSet<[u8; 32]>> {
    let mut removed = crate::fleet_removal::removed_device_ids(store, trust).await?;
    removed.remove(me);
    Ok(removed)
}

/// Read a nest's device roster on the account's session.
///
/// # Errors
///
/// The request failed.
pub async fn read_roster<R: RpcRequester>(rpc: &R) -> Result<Vec<SyncDevice>> {
    let reply: SyncDevicesListReply = rpc
        .request(
            KIND_DEVICES_LIST,
            SyncDevicesListRequest {
                extra: Default::default(),
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(reply.devices)
}

/// Revoke, by key, the grant of every `roster` row whose principal is in
/// `removed` (module docs, steps 3–5). `me` is never named, whatever
/// `removed` holds.
pub async fn revoke_listed<R>(
    rpc: &R,
    me: &[u8; 32],
    removed: &HashSet<[u8; 32]>,
    roster: &[SyncDevice],
) -> RemovedGrantsPass
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let mut pass = RemovedGrantsPass {
        roster_read: true,
        ..RemovedGrantsPass::default()
    };
    let mut asked = BTreeSet::new();
    for row in roster {
        let Some(key) = row
            .principal
            .as_deref()
            .and_then(|p| fauna_core::hex32::decode(p).ok())
        else {
            continue;
        };
        if key == *me || !removed.contains(&key) {
            continue;
        }
        if row.guardian_marked {
            pass.skipped_marked += 1;
            continue;
        }
        if !asked.insert(key) {
            continue;
        }
        let sent: Result<DeviceGrantRevokeReply, R::Error> = rpc
            .request(
                KIND_DEVICE_GRANT_REVOKE,
                DeviceGrantRevokeRequest {
                    device_key: fauna_core::hex32::encode(&key),
                    timestamp_ms: None,
                    nonce: None,
                    signature: None,
                    extra: Default::default(),
                },
            )
            .await;
        match sent {
            // `revoked: false` is success too: the key is tombstoned either way.
            Ok(_) => pass.revoked += 1,
            Err(e)
                if e.as_rpc_error()
                    .is_some_and(|r| r.code == CODE_GUARDIAN_MARKED) =>
            {
                pass.refused_marked += 1;
            }
            Err(e) => pass.errors.push(format!(
                "{KIND_DEVICE_GRANT_REVOKE} ({}): {e}",
                fauna_core::hex32::encode(&key)
            )),
        }
    }
    pass
}

/// What the caller already knows of this nest's roster this run.
pub enum Roster<'a> {
    /// Not read: the arm reads it, and only once merged state reads another
    /// device removed.
    Unread,
    /// Read already this run (the pump's staged-removal reconcile does).
    Read(&'a [SyncDevice]),
    /// Asked for already this run, and the nest did not answer: the arm does
    /// not ask twice.
    Unreadable(String),
}

/// The whole arm at one nest, over `rpc` — an authenticated session of the
/// account there (module docs).
pub async fn revoke_removed_grants<B, R>(
    store: &AccountStore<B>,
    trust: &GenerationTrust,
    me: &[u8; 32],
    rpc: &R,
    roster: Roster<'_>,
) -> RemovedGrantsPass
where
    B: StoreBackend,
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let removed = match removed_others(store, trust, me).await {
        Ok(r) => r,
        Err(e) => {
            return RemovedGrantsPass {
                errors: vec![format!("reading the removed members: {e:#}")],
                ..RemovedGrantsPass::default()
            };
        }
    };
    if removed.is_empty() {
        return RemovedGrantsPass::default();
    }
    match roster {
        Roster::Read(roster) => revoke_listed(rpc, me, &removed, roster).await,
        Roster::Unreadable(why) => RemovedGrantsPass::unread(why),
        Roster::Unread => match read_roster(rpc).await {
            Ok(roster) => revoke_listed(rpc, me, &removed, &roster).await,
            Err(e) => RemovedGrantsPass::unread(format!("{e:#}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation_fixture_test_support::*;
    use fauna_core::generation::DeviceSetRecord;
    use fauna_protocol::error::RpcError;
    use fauna_protocol::merge_policy::KIND_DEVICE_SET;
    use std::sync::{Arc, Mutex};

    /// A nest that serves `roster` (or, `unlisted`, refuses to), records every
    /// request it is sent, and answers a revoke of a key in `refuses` with the
    /// marker refusal.
    #[derive(Clone, Default)]
    struct Wire {
        roster: Vec<SyncDevice>,
        unlisted: bool,
        refuses: Vec<String>,
        sent: Arc<Mutex<Vec<Sent>>>,
    }

    /// One request the [`Wire`] saw: its kind and its canonical payload.
    type Sent = (&'static str, Vec<u8>);

    impl Wire {
        fn kinds(&self) -> Vec<&'static str> {
            self.sent.lock().unwrap().iter().map(|(k, _)| *k).collect()
        }
        fn revoked_keys(&self) -> Vec<String> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| *k == KIND_DEVICE_GRANT_REVOKE)
                .map(|(_, b)| {
                    let req: DeviceGrantRevokeRequest =
                        fauna_core::encoding::canonical_decode(b).unwrap();
                    assert!(
                        req.timestamp_ms.is_none()
                            && req.nonce.is_none()
                            && req.signature.is_none(),
                        "the app/user arm: no proof of possession"
                    );
                    req.device_key
                })
                .collect()
        }
    }

    #[derive(Debug)]
    struct WireErr(RpcError);
    impl std::fmt::Display for WireErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0.code)
        }
    }
    impl RpcErrorClass for WireErr {
        fn is_rejection(&self) -> bool {
            true
        }
        fn as_rpc_error(&self) -> Option<&RpcError> {
            Some(&self.0)
        }
    }

    impl RpcRequester for Wire {
        type Error = WireErr;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, WireErr>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_core::encoding::canonical_encode(&payload).unwrap();
            self.sent.lock().unwrap().push((kind, bytes.clone()));
            let reply = match kind {
                KIND_DEVICES_LIST if self.unlisted => {
                    return Err(WireErr(RpcError::new("fauna.sync.internal", "down")));
                }
                KIND_DEVICES_LIST => {
                    fauna_core::encoding::canonical_encode(&SyncDevicesListReply {
                        devices: self.roster.clone(),
                        extra: Default::default(),
                    })
                }
                KIND_DEVICE_GRANT_REVOKE => {
                    let req: DeviceGrantRevokeRequest =
                        fauna_core::encoding::canonical_decode(&bytes).unwrap();
                    if self.refuses.contains(&req.device_key) {
                        return Err(WireErr(RpcError::new(CODE_GUARDIAN_MARKED, "marked")));
                    }
                    fauna_core::encoding::canonical_encode(&DeviceGrantRevokeReply {
                        revoked: true,
                        sessions_revoked: 0,
                        extra: Default::default(),
                    })
                }
                other => return Err(WireErr(RpcError::new("kind_not_served", other))),
            }
            .unwrap();
            Ok(fauna_core::encoding::canonical_decode(&reply).unwrap())
        }
    }

    const THIRD: [u8; 32] = [0xC3u8; 32];

    /// A roster row named `label`, carrying `principal`.
    fn row(label: &str, principal: Option<[u8; 32]>, guardian_marked: bool) -> SyncDevice {
        SyncDevice {
            device_id: format!("{:0<64}", hex::encode(label.as_bytes())),
            label: label.into(),
            label_sealed: None,
            capabilities: "read,write".into(),
            registered_at: 1,
            last_seen_at: 1,
            online: false,
            principal: principal.map(|p| fauna_core::hex32::encode(&p)),
            folders: Vec::new(),
            guardian_marked,
            p2p_participation: None,
            p2p_off_requested: false,
            extra: Default::default(),
        }
    }

    /// Merged state reads `seed`'s device removed.
    async fn remove(f: &Fixture, seed: [u8; 32]) {
        f.put(enrollment_row(seed)).await;
        f.put(machinery_row(
            KIND_DEVICE_SET,
            fauna_core::hex32::encode(&device_id_of(seed)),
            &DeviceSetRecord::Removed {
                removed_at_ms: 9_000,
                removed_by: device_id_of(US),
            },
        ))
        .await;
    }

    /// Before the account's first removal the arm asks the nest nothing — not
    /// even its roster.
    #[tokio::test]
    async fn nothing_removed_sends_no_roster_request() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let wire = Wire {
            roster: vec![row("them", Some(device_id_of(THEM)), false)],
            ..Wire::default()
        };
        let pass =
            revoke_removed_grants(&f.store, &f.trust, &device_id_of(US), &wire, Roster::Unread)
                .await;
        assert_eq!(pass, RemovedGrantsPass::default());
        assert!(wire.kinds().is_empty(), "sent: {:?}", wire.kinds());
    }

    /// This device's own key is never named — when it is the only removed id,
    /// nothing is asked at all; beside another, only the other is revoked.
    #[tokio::test]
    async fn this_devices_own_key_is_never_named() {
        let f = fixture().await;
        remove(&f, US).await;
        let me = device_id_of(US);
        let wire = Wire {
            roster: vec![
                row("us", Some(me), false),
                row("them", Some(device_id_of(THEM)), false),
            ],
            ..Wire::default()
        };
        let pass = revoke_removed_grants(&f.store, &f.trust, &me, &wire, Roster::Unread).await;
        assert_eq!(pass, RemovedGrantsPass::default());
        assert!(wire.kinds().is_empty(), "sent: {:?}", wire.kinds());

        remove(&f, THEM).await;
        let pass = revoke_removed_grants(&f.store, &f.trust, &me, &wire, Roster::Unread).await;
        assert!(pass.roster_read);
        assert_eq!(pass.revoked, 1);
        assert_eq!(
            wire.revoked_keys(),
            vec![fauna_core::hex32::encode(&device_id_of(THEM))]
        );
    }

    /// Only a row claiming a removed principal is revoked: a live member's
    /// row, a keyless row and a row whose principal does not parse are left
    /// alone, and the row itself is never deleted.
    #[tokio::test]
    async fn only_rows_claiming_a_removed_principal_are_revoked_and_no_row_is_deleted() {
        let f = fixture().await;
        f.put(enrollment_row(THIRD)).await;
        remove(&f, THEM).await;
        let mut garbled = row("garbled", None, false);
        garbled.principal = Some("not hex".into());
        let wire = Wire {
            roster: vec![
                row("third", Some(device_id_of(THIRD)), false),
                row("keyless", None, false),
                garbled,
                row("them", Some(device_id_of(THEM)), false),
            ],
            ..Wire::default()
        };
        let pass =
            revoke_removed_grants(&f.store, &f.trust, &device_id_of(US), &wire, Roster::Unread)
                .await;
        assert_eq!(
            pass,
            RemovedGrantsPass {
                roster_read: true,
                revoked: 1,
                ..RemovedGrantsPass::default()
            }
        );
        assert_eq!(
            wire.kinds(),
            vec![KIND_DEVICES_LIST, KIND_DEVICE_GRANT_REVOKE],
            "one roster read, one key-addressed revoke, no row-addressed request"
        );
        assert_eq!(
            wire.revoked_keys(),
            vec![fauna_core::hex32::encode(&device_id_of(THEM))]
        );
    }

    /// A row the roster flags guardian-marked is skipped with nothing sent,
    /// and a nest that refuses the revoke as marked (an older listing) is
    /// counted, never an error.
    #[tokio::test]
    async fn a_guardian_marked_row_is_left_alone() {
        let f = fixture().await;
        remove(&f, THEM).await;
        remove(&f, THIRD).await;
        let third = fauna_core::hex32::encode(&device_id_of(THIRD));
        let wire = Wire {
            roster: vec![
                row("them", Some(device_id_of(THEM)), true),
                row("third", Some(device_id_of(THIRD)), false),
            ],
            refuses: vec![third.clone()],
            ..Wire::default()
        };
        let pass =
            revoke_removed_grants(&f.store, &f.trust, &device_id_of(US), &wire, Roster::Unread)
                .await;
        assert_eq!(
            pass,
            RemovedGrantsPass {
                roster_read: true,
                skipped_marked: 1,
                refused_marked: 1,
                ..RemovedGrantsPass::default()
            }
        );
        assert_eq!(wire.revoked_keys(), vec![third]);
    }

    /// A roster the caller already read this run is used as given: the arm
    /// reads no second one.
    #[tokio::test]
    async fn a_roster_the_caller_already_read_is_not_read_again() {
        let f = fixture().await;
        remove(&f, THEM).await;
        let wire = Wire::default();
        let roster = [row("them", Some(device_id_of(THEM)), false)];
        let pass = revoke_removed_grants(
            &f.store,
            &f.trust,
            &device_id_of(US),
            &wire,
            Roster::Read(&roster),
        )
        .await;
        assert_eq!(pass.revoked, 1);
        assert_eq!(wire.kinds(), vec![KIND_DEVICE_GRANT_REVOKE]);
    }

    /// A roster that cannot be read sends nothing and says why.
    #[tokio::test]
    async fn an_unreadable_roster_is_reported_and_nothing_is_revoked() {
        let f = fixture().await;
        remove(&f, THEM).await;
        let wire = Wire {
            unlisted: true,
            ..Wire::default()
        };
        let pass =
            revoke_removed_grants(&f.store, &f.trust, &device_id_of(US), &wire, Roster::Unread)
                .await;
        assert!(!pass.roster_read);
        assert_eq!(pass.errors.len(), 1, "{:?}", pass.errors);
        assert_eq!(wire.kinds(), vec![KIND_DEVICES_LIST]);

        // A caller that already asked this run and got no answer: not asked again.
        let wire = Wire::default();
        let pass = revoke_removed_grants(
            &f.store,
            &f.trust,
            &device_id_of(US),
            &wire,
            Roster::Unreadable("down".into()),
        )
        .await;
        assert!(!pass.roster_read);
        assert_eq!(pass.errors.len(), 1, "{:?}", pass.errors);
        assert!(wire.kinds().is_empty(), "sent: {:?}", wire.kinds());
    }
}
