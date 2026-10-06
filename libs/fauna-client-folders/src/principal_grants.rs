//! A third-party principal's folder read grants over a served set — their
//! renew on rotation and their revoke on unserve (`webdav-server.md` § Key
//! model → *A principal's read* rule (4)).
//!
//! Each twin is its own grant per (principal holder key, set), named by
//! [`fauna_client_capabilities::folder_principal_grant_id`] at the generation
//! the owner's log walks to; rule (4)'s finder is
//! [`fauna_client_capabilities::live_folder_principal_grants`] over that log,
//! so no roster read and no nest enumerate is needed. Two legs:
//!
//! - **Renew** — the serve reconcile ([`crate::reconcile_webdav_keys_blob`])
//!   re-sends every live twin over each served set the set's full generation
//!   bundle through [`renew_with_bundle`], the one renew send it shares with
//!   [`crate::orchestration::FoldersAuthor::rotate_paywall_grant`]; the nest
//!   dedups `(scope, epoch)`, so only a rotation's new generation lands. The
//!   window end is the grant's RECORDED one, never a slid one: a principal is
//!   never a blessed box, so a rotation adds keys and leaves the lapse where
//!   the consent put it.
//! - **Revoke** — the serve-off tail, before the key rotates: each twin is
//!   ended on the nest first (`fauna.capabilities.revoke`), then the signed
//!   `Revoke`s are recorded in ONE log write
//!   ([`fauna_client_capabilities::grant_log::record_revokes`]). The
//!   principal's records grant is a separate grant and is never touched.

use std::sync::Arc;

use fauna_client_capabilities::grant_log::{self, CurrentGrant, KeypairGrantEventSigner};
use fauna_client_capabilities::{MintGrantError, live_folder_principal_grants, mint_folder_grant};
use fauna_core::data::FoldersConfig;
use fauna_core::identity::ActorKeypair;
use fauna_mls::wrapped_blob::{GrantBlob, GrantWindow};
use fauna_protocol::RpcRequester;
use fauna_protocol::wrapped_blob::{
    RenewGrantReply, RenewGrantRequest, RevokeGrantReply, RevokeGrantRequest,
};
use zeroize::Zeroizing;

/// Why a folder grant's renew, or a principal twin's revoke, did not land.
#[derive(Debug)]
pub enum FolderGrantError<E> {
    /// Rebuilding the grant's wraps from custody failed.
    Mint(MintGrantError),
    /// The nest call failed.
    Transport(E),
    /// The owner's grant log could not be read.
    Log(fauna_client_config::StoreError),
    /// The signed `Revoke`s could not be recorded (the nest already ended the
    /// grants; the log's reconcile sweep converges the record).
    Record(grant_log::RecordRevokesError),
}

impl<E: core::fmt::Display> core::fmt::Display for FolderGrantError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Mint(e) => write!(f, "folder grant wraps: {e}"),
            Self::Transport(e) => write!(f, "folder grant transport: {e}"),
            Self::Log(e) => write!(f, "folder grant log read: {e}"),
            Self::Record(e) => write!(f, "folder grant revoke record: {e}"),
        }
    }
}

impl<E: core::fmt::Display + core::fmt::Debug> std::error::Error for FolderGrantError<E> {}

/// Why [`renew_with_bundle`] did not land: the two failures a renew can meet
/// (it reads no log and records nothing).
#[derive(Debug)]
pub(crate) enum RenewFailure<E> {
    Mint(MintGrantError),
    Transport(E),
}

impl<E> From<RenewFailure<E>> for FolderGrantError<E> {
    fn from(e: RenewFailure<E>) -> Self {
        match e {
            RenewFailure::Mint(e) => Self::Mint(e),
            RenewFailure::Transport(e) => Self::Transport(e),
        }
    }
}

/// **The one folder-grant renew send** — `blob` is the set's full generation
/// bundle rebuilt for the grant's holder ([`mint_folder_grant`]); its wraps go
/// as `appended_keys`, and the nest dedups `(scope, epoch)`, so only
/// generations newer than the stored grant land. Both the paywall grant
/// ([`crate::orchestration::FoldersAuthor::rotate_paywall_grant`], which slides
/// its window and records that `Renew` first) and a principal's twin
/// ([`PrincipalFolderGrants::renew_over`], at its recorded end) renew through
/// it.
pub(crate) async fn renew_with_bundle<R: RpcRequester>(
    requester: &R,
    blob: &GrantBlob,
    grant_id: [u8; 16],
    new_epoch_end: u64,
) -> Result<(), RenewFailure<R::Error>> {
    let appended_keys: Vec<fauna_protocol::ByteBuf> = blob
        .wrapped_keys
        .iter()
        .map(|k| {
            k.to_canonical_bytes()
                .map(fauna_protocol::ByteBuf::from)
                .map_err(|e| RenewFailure::Mint(MintGrantError::Wrap(e)))
        })
        .collect::<Result<_, _>>()?;
    let _: RenewGrantReply = requester
        .request(
            "fauna.capabilities.renew",
            RenewGrantRequest {
                grant_id: fauna_protocol::ByteBuf::from(grant_id.to_vec()),
                // A folder grant's wraps are generation-indexed, not calendar
                // epochs: nothing to retain by date, so the start stays where
                // the mint put it.
                new_epoch_start: None,
                new_epoch_end,
                appended_keys,
                extra: Default::default(),
            },
        )
        .await
        .map_err(RenewFailure::Transport)?;
    Ok(())
}

/// The owner's handle on its principals' folder read twins: the grant log
/// (the finder's input and the `Revoke`s' home) and the identity whose secret
/// derives every twin's id and whose key signs the `Revoke`s. Cheap to clone.
#[derive(Clone)]
pub struct PrincipalFolderGrants {
    ledger: Arc<dyn fauna_client_config::SuccessionLedgerStore>,
    owner_secret: Zeroizing<[u8; 32]>,
}

impl PrincipalFolderGrants {
    /// Over the account's grant-log store, acting as `owner`.
    pub fn new(
        ledger: Arc<dyn fauna_client_config::SuccessionLedgerStore>,
        owner: &ActorKeypair,
    ) -> Self {
        Self {
            ledger,
            owner_secret: Zeroizing::new(*owner.secret_bytes()),
        }
    }

    fn owner(&self) -> ActorKeypair {
        ActorKeypair::from_secret(*self.owner_secret)
    }

    /// Every principal's live twin over `set_name`, read from the log.
    async fn live_over<E>(&self, set_name: &str) -> Result<Vec<CurrentGrant>, FolderGrantError<E>> {
        let log = self.ledger.load().await.map_err(FolderGrantError::Log)?;
        Ok(live_folder_principal_grants(
            &log.grant_events,
            &self.owner_secret,
            set_name,
        ))
    }

    /// Renew every principal's live twin over `set_name` with the set's full
    /// generation bundle, at the grant's recorded window end. A lapsed twin
    /// is skipped (it reads nothing; a new approve re-mints it). Returns how
    /// many renews were sent; the first failure stops the pass (the next
    /// reconcile re-sends — the nest dedups).
    pub async fn renew_over<R: RpcRequester>(
        &self,
        requester: &R,
        custody: &FoldersConfig,
        set_name: &str,
        custody_channel: &[u8; 32],
        now_secs: u64,
    ) -> Result<usize, FolderGrantError<R::Error>> {
        let owner = self.owner().actor_id().0;
        let mut renewed = 0;
        for grant in self.live_over(set_name).await? {
            let (Ok(grant_id), Ok(holder)) = (
                <[u8; 16]>::try_from(grant.grant_id.as_slice()),
                <[u8; 32]>::try_from(grant.holder.as_slice()),
            ) else {
                continue;
            };
            if grant.window_end <= now_secs {
                continue;
            }
            let blob = mint_folder_grant(
                custody,
                &owner,
                &grant_id,
                &holder,
                // A consent's twin is sealed classical-only
                // (`prepare_ext_consent_grant`); its renew matches.
                None,
                GrantWindow(grant.window_start, grant.window_end),
                set_name,
                custody_channel,
            )
            .map_err(FolderGrantError::Mint)?;
            // The window is the recorded one, so the log gains no `Renew`:
            // a key-only refresh changes nothing the trust facet reads, and
            // this pass runs at every reconcile.
            renew_with_bundle(requester, &blob, grant_id, grant.window_end).await?;
            renewed += 1;
        }
        Ok(renewed)
    }

    /// Revoke every principal's live twin over `set_name` — the nest first,
    /// one `fauna.capabilities.revoke` per grant id, then every signed
    /// `Revoke` in one log write. Runs before the serve-off rotation, so a
    /// failure leaves the set served and the rotation owed (the caller
    /// surfaces it; the launch pass's unserve arm re-drives the whole tail).
    /// Returns how many twins were revoked.
    pub async fn revoke_over<R: RpcRequester>(
        &self,
        requester: &R,
        set_name: &str,
        now_secs: u64,
    ) -> Result<usize, FolderGrantError<R::Error>> {
        let mut ended = Vec::new();
        let mut refused = None;
        for grant in self.live_over(set_name).await? {
            let (Ok(grant_id), Ok(holder)) = (
                <[u8; 16]>::try_from(grant.grant_id.as_slice()),
                <[u8; 32]>::try_from(grant.holder.as_slice()),
            ) else {
                continue;
            };
            let sent: Result<RevokeGrantReply, _> = requester
                .request(
                    "fauna.capabilities.revoke",
                    RevokeGrantRequest {
                        grant_id: fauna_protocol::ByteBuf::from(grant_id.to_vec()),
                        extra: Default::default(),
                    },
                )
                .await;
            match sent {
                Ok(_) => ended.push((grant_id, holder)),
                Err(e) => {
                    refused = Some(e);
                    break;
                }
            }
        }
        // What the nest already ended is recorded even when a later revoke
        // failed, so the log never holds live a grant the nest dropped.
        let owner = self.owner();
        grant_log::record_revokes(
            &*self.ledger,
            &KeypairGrantEventSigner::new(&owner),
            owner.actor_id().0,
            &ended,
            now_secs,
        )
        .await
        .map_err(FolderGrantError::Record)?;
        match refused {
            Some(e) => Err(FolderGrantError::Transport(e)),
            None => Ok(ended.len()),
        }
    }
}
